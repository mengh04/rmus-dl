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
