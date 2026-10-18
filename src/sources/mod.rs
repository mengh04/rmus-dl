pub mod kugou;
pub mod kuwo;

use std::path::PathBuf;

use anyhow::Result;
use reqwest::Url;

/// 极简 percent-encoding，仅用于拼搜索 URL 的关键词。
pub(crate) fn urlencode(input: &str) -> String {
    let mut out = String::new();
    for byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// 酷我搜索结果里的 MINFO 字段：分号分隔的音质清单，每段形如
/// `level:ff,bitrate:2000,format:flac,size:52.83Mb`。解析出下载前就能
/// 告知用户的品质信息（是否无损、最优 mp3 档位、体积估计）。
///
/// 注意 size 的单位标注是 Mb 但数值更接近 MB，且与真实码率对不严格——
/// 只作展示用途；权威大小以下载前的 Range 探针为准。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QualityInfo {
    /// 有无损（format:flac，通常是 level:ff）
    pub flac: bool,
    /// 无损档的体积估计（字节）；来源标注不可解析时为 None
    pub flac_size: Option<u64>,
    /// 最优 mp3 的码率标签（"320k" / "192k" / "128k"）；无 mp3 为 None
    pub mp3_label: Option<String>,
}

/// Range 探针结果：真实格式与精确总大小。
pub struct Probe {
    pub is_flac: bool,
    pub total: Option<u64>,
}

/// 下载前的最后确认：取 URL 开头几十字节看魔数，顺带从 Content-Range
/// 拿精确总大小。任何 CDN 忽略 Range 返回 200 时也只读第一个分块就断开，
/// 不会把整份文件拉下来。
///
/// `expect_flac` 时魔数不是 fLaC 直接报错——这是“只要无损”的最终闸门，
/// 上游解析站偷偷降级 mp3 也会在这里被拦下，而不是下完才发现。
pub async fn probe_audio(url: &Url, expect_flac: bool) -> anyhow::Result<Probe> {
    use reqwest::header::CONTENT_RANGE;

    let mut resp = reqwest::Client::new()
        .get(url.clone())
        .header("Range", "bytes=0-15")
        .header("User-Agent", "Mozilla/5.0")
        .send()
        .await?
        .error_for_status()?;

    let mut head: Vec<u8> = Vec::new();
    while head.len() < 16
        && let Some(chunk) = resp.chunk().await?
    {
        let want = 16 - head.len();
        head.extend_from_slice(&chunk[..chunk.len().min(want)]);
        if head.len() >= 16 {
            break;
        }
    }

    let is_flac = head.starts_with(b"fLaC");
    if expect_flac && !is_flac {
        let got = if head.starts_with(b"ID3") || head.first().is_some_and(|b| b & 0xE0 == 0xE0) {
            "mp3"
        } else {
            "未知格式"
        };
        anyhow::bail!("上游返回的不是 FLAC（实际 {got}），已取消下载");
    }
    let total = resp
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.rsplit('/').next().and_then(|s| s.parse::<u64>().ok()));

    Ok(Probe { is_flac, total })
}

pub(crate) fn parse_minfo(minfo: &str) -> QualityInfo {
    let mut quality = QualityInfo { flac: false, flac_size: None, mp3_label: None };
    for entry in minfo.split(';') {
        let mut level = "";
        let mut format = "";
        let mut bitrate: u64 = 0;
        let mut size_mb: Option<f64> = None;
        for pair in entry.split(',') {
            let Some((key, value)) = pair.split_once(':') else { continue };
            match key.trim() {
                "level" => level = value.trim(),
                "format" => format = value.trim(),
                "bitrate" => bitrate = value.trim().parse().unwrap_or(0),
                "size" => {
                    size_mb = value.trim().strip_suffix("Mb").and_then(|s| s.parse().ok());
                }
                _ => {}
            }
        }
        match format {
            "flac" => {
                quality.flac = true;
                if quality.flac_size.is_none() {
                    quality.flac_size = size_mb.map(|mb| (mb * 1024.0 * 1024.0) as u64);
                }
            }
            "mp3" => {
                let better = quality
                    .mp3_label
                    .as_deref()
                    .and_then(|label| label.strip_suffix('k'))
                    .and_then(|k| k.parse::<u64>().ok())
                    .is_none_or(|best| bitrate > best);
                if better && bitrate > 0 {
                    quality.mp3_label = Some(format!("{bitrate}k"));
                }
            }
            _ => {}
        }
        let _ = level; // level 字段（ff/p/h/s）暂不参与判定，format 已足够
    }
    quality
}

/// 一条搜索结果：足够展示 + 回源取下载链接和元数据。
#[derive(Debug, Clone)]
pub struct SearchResult {
    /// 展示用标题，如 "晴天 - 周杰伦"
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// 源内唯一标识（酷我 rid / 酷狗 hash）
    pub song_id: String,
    /// 源名，用于回源
    pub source: &'static str,
    /// 搜索结果自带的封面（酷狗有，可含 {size} 占位符；酷我从详情接口拿）
    pub cover_url: Option<String>,
    /// 时长（毫秒）；搜索接口给不出时为 0
    pub duration_ms: u64,
    /// 下载前已知的最优格式标签："flac" / "320k" / "128k"；未知为 None
    pub format: Option<String>,
    /// 该格式对应的文件大小（字节）；未知为 None
    pub size_bytes: Option<u64>,
}

/// 下载所需的补充元数据（歌词、封面）。
#[derive(Debug, Default)]
pub struct SongMetadata {
    /// LRC 格式歌词
    pub lyric: Option<String>,
    pub cover_url: Option<Url>,
}

impl SongMetadata {
    /// 门禁：歌词和封面至少要拿到一样，否则这次下载算失败。
    pub fn gate(&self) -> Result<()> {
        if self.lyric.is_none() && self.cover_url.is_none() {
            anyhow::bail!("metadata gate failed: neither lyric nor cover was resolved");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_gate_requires_lyric_or_cover() {
        assert!(SongMetadata::default().gate().is_err());
        assert!(
            SongMetadata {
                lyric: Some("[00:00.00]x".into()),
                cover_url: None,
            }
            .gate()
            .is_ok()
        );
        assert!(
            SongMetadata {
                lyric: None,
                cover_url: Some(Url::parse("http://example.com/a.jpg").unwrap()),
            }
            .gate()
            .is_ok()
        );
    }

    #[test]
    fn minfo_解析出无损与最优mp3() {
        let q = parse_minfo(
            "level:ff,bitrate:2000,format:flac,size:52.83Mb;level:p,bitrate:192,format:ogg,size:5.59Mb;level:p,bitrate:300,format:ogg,size:9.85Mb;level:h,bitrate:100,format:ogg,size:2.79Mb;level:p,bitrate:320,format:mp3,size:10.29Mb;level:s,bitrate:48,format:aac,size:1.57Mb;level:h,bitrate:128,format:mp3,size:4.12Mb",
        );
        assert!(q.flac);
        assert_eq!(q.flac_size, Some((52.83 * 1024.0 * 1024.0) as u64));
        assert_eq!(q.mp3_label.as_deref(), Some("320k"));
    }

    #[test]
    fn minfo_没有无损时只报mp3() {
        let q = parse_minfo("level:p,bitrate:320,format:mp3,size:10.29Mb;level:h,bitrate:128,format:mp3,size:4.12Mb");
        assert!(!q.flac);
        assert_eq!(q.flac_size, None);
        assert_eq!(q.mp3_label.as_deref(), Some("320k"));
    }

    #[test]
    fn minfo_空串与坏字段不炸() {
        let q = parse_minfo("");
        assert!(!q.flac);
        assert_eq!(q.mp3_label, None);
        let q = parse_minfo("level:ff,format:flac,size:xxMb;format:mp3");
        assert!(q.flac);
        assert_eq!(q.flac_size, None);
        assert_eq!(q.mp3_label, None);
    }
}

/// 一个音乐源。仅在本 crate 内部使用，async fn in trait 的 Send 限制
/// 不影响（app 按源依次调用）。
#[allow(async_fn_in_trait)]
pub trait MusicSource: Send + Sync {
    fn name(&self) -> &'static str;
    async fn search(&self, keyword: &str) -> Result<Vec<SearchResult>>;
    async fn get_download_url(&self, song: &SearchResult) -> Result<Url>;
    async fn get_metadata(&self, song: &SearchResult) -> Result<SongMetadata>;
}

/// 把歌词和封面写进音频文件的 tag。
/// 门禁：没有歌词也没有封面时直接失败；写入后读回验证，防止静默丢失。
pub async fn write_metadata(file_path: &PathBuf, meta: &SongMetadata) -> Result<()> {
    use lofty::{
        config::WriteOptions,
        file::{AudioFile, TaggedFileExt},
        picture::{Picture, PictureType},
        tag::ItemKey,
    };
    use std::io::Cursor;

    meta.gate()?;

    let mut tagged_file = lofty::read_from_path(file_path)?;
    let Some(tag) = tagged_file.primary_tag_mut() else {
        anyhow::bail!("No primary tag found in the audio file");
    };

    // ItemKey::Lyrics 在 ID3v2（mp3）下没有对应帧，保存时会被静默丢弃，
    // 必须用 UnsyncLyrics（USLT）；FLAC 的 Vorbis comment 习惯用 LYRICS 字段。
    let lyrics_key = match tag.tag_type() {
        lofty::tag::TagType::Id3v2 => ItemKey::UnsyncLyrics,
        _ => ItemKey::Lyrics,
    };

    if let Some(lyric) = &meta.lyric {
        tag.insert_text(lyrics_key, lyric.clone());
    }

    if let Some(cover_url) = &meta.cover_url {
        let cover_bytes = reqwest::get(cover_url.clone()).await?.bytes().await?;
        let mut cursor = Cursor::new(cover_bytes.to_vec());
        let mut picture = Picture::from_reader(&mut cursor)?;
        picture.set_pic_type(PictureType::CoverFront);
        tag.remove_picture_type(PictureType::CoverFront);
        tag.push_picture(picture);
    }

    tagged_file.save_to_path(file_path, WriteOptions::default())?;

    // 门禁复核：重新读文件，确认要求写入的内容真的在 tag 里
    let verified = lofty::read_from_path(file_path)?;
    let Some(tag) = verified.primary_tag() else {
        anyhow::bail!("verification failed: no primary tag after save");
    };
    if meta.lyric.is_some() && tag.get_items(lyrics_key).next().is_none() {
        anyhow::bail!("verification failed: LYRICS missing from tag after save");
    }
    if meta.cover_url.is_some() && tag.pictures().is_empty() {
        anyhow::bail!("verification failed: picture missing from tag after save");
    }
    Ok(())
}
