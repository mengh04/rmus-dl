//! 酷狗音乐源：官方搜索接口 + baka.plus 第三方解析下载 + 官方 krcs 歌词接口。
//! 接口用法抄自 musicdl (CharlesPikachu/musicdl) 的 kugou.py。

use anyhow::{Context, Result, bail};
use base64::Engine;
use reqwest::{Client, Url, header::HeaderValue};
use serde::Deserialize;

use super::{MusicSource, SearchResult, SongMetadata, urlencode};

const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/134.0.0.0 Safari/537.36";

/// 可克隆（reqwest Client 本就是 Arc 内核）：插件模式下方便把源
/// 克隆进后台下载任务。
#[derive(Clone)]
pub struct Kugou {
    client: Client,
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    data: Option<SearchData>,
}

#[derive(Debug, Deserialize)]
struct SearchData {
    #[serde(default)]
    lists: Vec<SearchItem>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct SearchItem {
    #[serde(rename = "FileHash")]
    file_hash: String,
    song_name: Option<String>,
    singer_name: Option<String>,
    album_name: Option<String>,
    file_name: Option<String>,
    /// 封面，一般带 {size} 占位符
    image: Option<String>,
    #[serde(rename = "trans_param")]
    trans_param: Option<TransParam>,
}

#[derive(Debug, Deserialize)]
struct TransParam {
    union_cover: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LyricSearchResponse {
    candidates: Option<Vec<LyricCandidate>>,
}

#[derive(Debug, Deserialize)]
struct LyricCandidate {
    /// 注意：JSON 里是字符串形式的数字
    id: String,
    accesskey: String,
}

#[derive(Debug, Deserialize)]
struct LyricDownloadResponse {
    content: Option<String>,
}

/// meting 式解析站（tom.moe / 90svip）的 POST 响应。
#[derive(Debug, Deserialize)]
struct MetingResolveResponse {
    data: Option<Vec<MetingItem>>,
    code: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct MetingItem {
    url: Option<String>,
}

impl Kugou {
    pub fn new() -> Result<Self> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("User-Agent", HeaderValue::from_static(USER_AGENT));
        let client = Client::builder()
            .cookie_store(true)
            .default_headers(headers)
            .build()?;
        Ok(Self { client })
    }
}

impl MusicSource for Kugou {
    fn name(&self) -> &'static str {
        "kugou"
    }

    async fn search(&self, keyword: &str) -> Result<Vec<SearchResult>> {
        let url = format!(
            "https://songsearch.kugou.com/song_search_v2?keyword={}&platform=WebFilter&page=1&pagesize=30",
            urlencode(keyword)
        );
        let resp: SearchResponse = self
            .client
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
            .context("parse kugou search response")?;
        let lists = resp.data.map(|d| d.lists).unwrap_or_default();
        Ok(lists
            .into_iter()
            .filter(|item| !item.file_hash.is_empty())
            .map(|item| {
                let cover_url = item
                    .trans_param
                    .and_then(|tp| tp.union_cover)
                    .or(item.image)
                    .map(|u| u.replace("{size}", "400"));
                SearchResult {
                    title: item.song_name.clone().unwrap_or_else(|| {
                        item.file_name.clone().unwrap_or_else(|| item.file_hash.clone())
                    }),
                    artist: item.singer_name,
                    album: item.album_name,
                    song_id: item.file_hash,
                    source: "kugou",
                    cover_url,
                }
            })
            .collect())
    }

    /// 解析下载直链：降级链 baka（GET，重试 2 次）→ tom.moe → 90svip
    /// （后两者是 musicdl 同款 meting 式 POST 解析站）。最终 URL 统一降为
    /// http：酷狗 CDN 的 https 证书域名经常不匹配。
    async fn get_download_url(&self, song: &SearchResult) -> Result<Url> {
        let mut last_err = None;

        // 1) baka：GET 302 到酷狗 CDN
        for attempt in 0..2 {
            let url = format!(
                "https://api.baka.plus/meting/?server=kugou&type=url&id={}&br=2000",
                song.song_id
            );
            match self
                .client
                .get(&url)
                .header("User-Agent", USER_AGENT)
                .send()
                .await
            {
                Ok(resp) if resp.url().host_str() != Some("api.baka.plus") => {
                    return Ok(downgrade_to_http(resp.url()));
                }
                Ok(resp) => {
                    last_err = Some(anyhow::anyhow!(
                        "baka did not redirect (status {})",
                        resp.status()
                    ));
                }
                Err(e) => last_err = Some(e.into()),
            }
            if attempt == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(800)).await;
            }
        }

        // 2) meting 式解析站
        for base in ["https://music.tom.moe/", "https://music.90svip.cn/"] {
            match resolve_with_meting_site(&self.client, base, &song.song_id).await {
                Ok(url) => return Ok(downgrade_to_http(&url)),
                Err(e) => {
                    eprintln!("kugou: meting site {base} failed: {e}");
                    last_err = Some(e);
                }
            }
        }

        bail!(
            "kugou: all resolvers failed for {}: {last_err:?}",
            song.title
        )
    }

    async fn get_metadata(&self, song: &SearchResult) -> Result<SongMetadata> {
        // 歌词：krcs 搜索候选，再下载 base64 的 lrc
        let keyword = song
            .title
            .split('-')
            .next()
            .unwrap_or(&song.title)
            .trim()
            .to_string();
        let search_url = format!(
            "https://krcs.kugou.com/search?ver=1&man=yes&client=mobi&keyword={}&duration=-1&hash={}",
            urlencode(&keyword),
            song.song_id
        );
        let lyric = match self
            .client
            .get(search_url)
            .send()
            .await?
            .error_for_status()?
            .json::<LyricSearchResponse>()
            .await
            .ok()
            .and_then(|r| r.candidates)
            .and_then(|c| c.into_iter().next())
        {
            Some(candidate) => {
                let download_url = format!(
                    "https://lyrics.kugou.com/download?ver=1&client=pc&id={}&accesskey={}&fmt=lrc&charset=utf8",
                    candidate.id, candidate.accesskey
                );
                self.client
                    .get(download_url)
                    .send()
                    .await?
                    .error_for_status()?
                    .json::<LyricDownloadResponse>()
                    .await
                    .ok()
                    .and_then(|r| r.content)
                    .and_then(|content| {
                        base64::engine::general_purpose::STANDARD
                            .decode(content)
                            .ok()
                            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                    })
            }
            None => None,
        };

        let cover_url = song
            .cover_url
            .as_deref()
            .and_then(|u| Url::parse(u).ok());
        Ok(SongMetadata {
            lyric,
            cover_url,
        })
    }
}

/// meting 式解析站：POST hash 拿相对 url，HEAD 跟随重定向到 CDN 直链。
async fn resolve_with_meting_site(client: &Client, base: &str, hash: &str) -> Result<Url> {
    let resp = client
        .post(base)
        .header("Referer", base)
        .header("X-Requested-With", "XMLHttpRequest")
        .form(&[
            ("input", hash),
            ("filter", "id"),
            ("type", "kugou"),
            ("page", "1"),
        ])
        .send()
        .await?
        .error_for_status()?;
    let parsed: MetingResolveResponse = resp.json().await?;
    if parsed.code == Some(0) {
        bail!("meting site {base} returned code 0");
    }
    let relative = parsed
        .data
        .unwrap_or_default()
        .into_iter()
        .find_map(|item| item.url)
        .filter(|u| !u.is_empty())
        .ok_or_else(|| anyhow::anyhow!("meting site {base} returned no url"))?;
    let full = Url::parse(base)?.join(&relative)?;
    // 跟随重定向拿 CDN 直链（不下载 body）
    let head = client
        .head(full)
        .header("User-Agent", USER_AGENT)
        .send()
        .await?
        .error_for_status()?;
    Ok(head.url().clone())
}

/// https → http 降级：酷狗部分 CDN 节点的证书不覆盖自己的域名。
fn downgrade_to_http(url: &Url) -> Url {
    let mut out = url.clone();
    if out.scheme() == "https" {
        out.set_scheme("http").ok();
    }
    out
}
