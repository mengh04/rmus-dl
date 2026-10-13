//! 酷我音乐源：官方搜索接口 + mobi.s DES 加密下载解析 + h5 歌词/封面接口。
//! 接口用法抄自 musicdl (CharlesPikachu/musicdl) 的 kuwo.py。

pub mod des;

use anyhow::{Context, Result, bail};
use reqwest::{Client, Url, header::HeaderValue};
use serde::Deserialize;

use super::{MusicSource, SearchResult, SongMetadata, urlencode};

const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/134.0.0.0 Safari/537.36";

pub struct Kuwo {
    client: Client,
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    abslist: Vec<SearchItem>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
struct SearchItem {
    #[serde(rename = "MUSICRID")]
    musicrid: String,
    name: Option<String>,
    artist: Option<String>,
    album: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SongInfoResponse {
    data: Option<SongInfoData>,
}

#[derive(Debug, Deserialize)]
struct SongInfoData {
    songinfo: Option<SongInfo>,
    lrclist: Option<Vec<LrcLine>>,
}

#[derive(Debug, Deserialize)]
struct SongInfo {
    pic: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LrcLine {
    #[serde(rename = "lineLyric")]
    line_lyric: String,
    time: String,
}

impl Kuwo {
    pub fn new() -> Result<Self> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("User-Agent", HeaderValue::from_static(USER_AGENT));
        let client = Client::builder()
            .cookie_store(true)
            .default_headers(headers)
            .build()?;
        Ok(Self { client })
    }

    /// mobi.s 下载接口：DES 加密的 query 换取直链，响应是 key=value 文本。
    async fn parse_download_url(&self, rid: &str, format: &str) -> Result<Url> {
        let query = format!(
            "user=0&corp=kuwo&source=kwplayer_ar_5.1.0.0_B_jiakong_vh.apk&p2p=1&type=convert_url2&sig=0&format={format}&rid={rid}"
        );
        let q = des::encrypt_query(&query);
        let resp = self
            .client
            .get(format!("http://mobi.kuwo.cn/mobi.s?f=kuwo&q={q}"))
            .header("User-Agent", "okhttp/3.10.0")
            .send()
            .await?
            .error_for_status()?;
        let text = resp.text().await?;
        for line in text.lines() {
            if let Some(url) = line.strip_prefix("url=") {
                if url.starts_with("http") {
                    return Ok(Url::parse(url)?);
                }
            }
        }
        bail!("mobi.s returned no url for rid {rid} format {format}:\n{text}");
    }
}

impl MusicSource for Kuwo {
    fn name(&self) -> &'static str {
        "kuwo"
    }

    async fn search(&self, keyword: &str) -> Result<Vec<SearchResult>> {
        let url = format!(
            "https://www.kuwo.cn/search/searchMusicBykeyWord?vipver=1&client=kt&ft=music&cluster=0&strategy=2012&encoding=utf8&rformat=json&mobi=1&issubtitle=1&show_copyright_off=1&pn=0&rn=30&all={}",
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
            .context("parse kuwo search response")?;
        Ok(resp
            .abslist
            .into_iter()
            .filter_map(|item| {
                let rid = item.musicrid.strip_prefix("MUSIC_")?.to_string();
                Some(SearchResult {
                    title: item.name.clone().unwrap_or_default(),
                    artist: item.artist,
                    album: item.album,
                    song_id: rid,
                    source: "kuwo",
                    cover_url: None,
                })
            })
            .collect())
    }

    async fn get_download_url(&self, song: &SearchResult) -> Result<Url> {
        // 先尝试无损 flac，失败再降级 mp3
        for format in ["flac", "mp3"] {
            match self.parse_download_url(&song.song_id, format).await {
                Ok(url) => return Ok(url),
                Err(e) => eprintln!("kuwo: format {format} failed for {}: {e}", song.title),
            }
        }
        bail!("kuwo: no download url for {}", song.title)
    }

    async fn get_metadata(&self, song: &SearchResult) -> Result<SongMetadata> {
        let url = format!(
            "https://m.kuwo.cn/newh5/singles/songinfoandlrc?musicId={}",
            song.song_id
        );
        // 接口偶发返回空 data / 业务码 301，重试三次（间隔递增）。
        // 注意：带 iPhone UA + Referer 反而容易被风控，这里只带默认浏览器 UA。
        // 拿不到就是失败——元数据是下载成功的门禁，不再静默降级。
        let mut last_err = anyhow::anyhow!("kuwo songinfo: empty data after 3 attempts");
        for attempt in 0..3 {
            match self
                .client
                .get(&url)
                .send()
                .await?
                .error_for_status()?
                .json::<SongInfoResponse>()
                .await
            {
                Ok(r) => match r.data {
                    Some(data) => {
                        let lyric = data.lrclist.map(|lines| {
                            lines
                                .iter()
                                .map(|l| format!("[{}]{}", to_lrc_time(&l.time), l.line_lyric))
                                .collect::<Vec<_>>()
                                .join("\r\n")
                        });
                        let cover_url = data
                            .songinfo
                            .and_then(|info| info.pic)
                            .and_then(|pic| Url::parse(&pic).ok());
                        return Ok(SongMetadata {
                            lyric,
                            cover_url,
                        });
                    }
                    None => {
                        last_err = anyhow::anyhow!("kuwo songinfo: empty data (attempt {})", attempt + 1);
                    }
                },
                Err(e) => {
                    last_err = anyhow::Error::from(e).context("parse kuwo songinfo response");
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(800 * (attempt as u64 + 1))).await;
        }
        Err(last_err)
    }
}

/// "123.45"（秒）→ "02:03.45"
fn to_lrc_time(time: &str) -> String {
    let secs: f64 = time.parse().unwrap_or(0.0);
    let total_ms = (secs * 1000.0).round() as u64;
    format!(
        "{:02}:{:02}.{:02}",
        total_ms / 60000,
        (total_ms / 1000) % 60,
        (total_ms % 1000) / 10
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lrc_time_format() {
        assert_eq!(to_lrc_time("0.0"), "00:00.00");
        assert_eq!(to_lrc_time("123.45"), "02:03.45");
        assert_eq!(to_lrc_time("2.25"), "00:02.25");
    }
}
