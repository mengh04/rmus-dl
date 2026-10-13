use std::{path::PathBuf, time::Duration};
use tokio::io::AsyncWriteExt;

use indicatif::{ProgressBar, ProgressStyle};
use lofty::{file::TaggedFileExt, tag::Accessor};

/// 下载音频文件。格式按文件头自动识别（flac / mp3，酷狗的解析站可能给
/// 128k mp3），文件名优先用音频自带 tag 的 title/artist，没有 tag
/// （酷我 CDN 直链常见）时退回 fallback_name。
pub async fn download_file(url: reqwest::Url, fallback_name: &str) -> anyhow::Result<PathBuf> {
    let mut response = reqwest::get(url).await?.error_for_status()?;

    let total_size = response.content_length();

    let progress = total_size
        .map(ProgressBar::new)
        .unwrap_or_else(ProgressBar::new_spinner);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("{bar:40.cyan/blue} {bytes}/{total_bytes} ({binary_bytes_per_sec}, {eta})")
            .unwrap()
            .progress_chars("=>-"),
    );
    progress.enable_steady_tick(Duration::from_millis(100));

    let temp_path = PathBuf::from("temp.audio.part");

    let mut file = tokio::fs::File::create(&temp_path).await?;
    while let Some(chunk) = response.chunk().await? {
        file.write_all(&chunk).await?;
        progress.inc(chunk.len() as u64);
    }

    file.flush().await?;
    drop(file);

    progress.finish_with_message("Download complete");

    let final_path = build_final_path(&temp_path, fallback_name)?;

    tokio::fs::rename(&temp_path, &final_path).await?;

    Ok(final_path)
}

/// 依据文件头魔数定扩展名，再读 tag 决定文件名。
/// 显式指定 FileType：temp 文件没有扩展名，lofty 自行探测会失败。
fn build_final_path(temp_path: &std::path::Path, fallback_name: &str) -> anyhow::Result<PathBuf> {
    use lofty::file::FileType;
    use std::io::Read;

    let mut magic = [0u8; 4];
    std::fs::File::open(temp_path)?.read_exact(&mut magic)?;
    let ext = match &magic {
        b"fLaC" => ("flac", FileType::Flac),
        m if &m[..3] == b"ID3" || (m[0] == 0xFF && m[1] & 0xE0 == 0xE0) => {
            ("mp3", FileType::Mpeg)
        }
        _ => anyhow::bail!("unrecognized audio format (magic: {magic:?})"),
    };

    let input = std::fs::File::open(temp_path)?;
    let tagged_file = lofty::probe::Probe::with_file_type(input, ext.1).read()?;
    let final_path = if let Some(tag) = tagged_file.primary_tag()
        && let (Some(title), Some(artist)) = (tag.title(), tag.artist())
    {
        PathBuf::from(format!("{} - {}.{}", title, artist, ext.0))
    } else {
        PathBuf::from(format!("{}.{}", sanitize_filename(fallback_name), ext.0))
    };
    drop(tagged_file);
    Ok(final_path)
}

/// 去掉文件名里的路径分隔符等常见非法字符。
fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn sanitize_keeps_cjk_and_replaces_separators() {
        assert_eq!(super::sanitize_filename("晴天 - 周杰伦"), "晴天 - 周杰伦");
        assert_eq!(super::sanitize_filename("a/b\\c:d"), "a_b_c_d");
    }
}
