use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

use lofty::{file::TaggedFileExt, tag::Accessor};

/// 下载参数：产物目录、进度回调、取消探测。
///
/// 进度回调每收到一块数据调一次，参数是 (本次块大小, 总大小若已知)；
/// 取消探测在下块之间轮询，返回 true 时放弃并删除临时文件。
pub struct DownloadOptions<'a> {
    /// 产物目录（插件模式 = rmusd 的库根目录；CLI 模式 = 当前目录）
    pub dest_dir: &'a Path,
    pub on_chunk: &'a (dyn Fn(u64, Option<u64>) + Send + Sync),
    pub cancelled: &'a (dyn Fn() -> bool + Send + Sync),
}

impl<'a> DownloadOptions<'a> {
    /// CLI 的默认选项：不做取消，进度回调留给调用方。
    pub fn quiet(dest_dir: &'a Path) -> Self {
        Self {
            dest_dir,
            on_chunk: &|_, _| {},
            cancelled: &|| false,
        }
    }
}

/// 下载音频文件。格式按文件头自动识别（flac / mp3，酷狗的解析站可能给
/// 128k mp3），文件名优先用音频自带 tag 的 title/artist，没有 tag
/// （酷我 CDN 直链常见）时退回 fallback_name。
///
/// 先写隐藏临时文件（避免并发任务互相覆盖，也不会被音乐库扫描到），
/// 成功后原子改名落到 `dest_dir`。
pub async fn download_file(
    url: reqwest::Url,
    fallback_name: &str,
    opts: &DownloadOptions<'_>,
) -> anyhow::Result<PathBuf> {
    let mut response = reqwest::get(url).await?.error_for_status()?;

    let total_size = response.content_length();
    let temp_path = temp_path(opts.dest_dir, fallback_name);
    let mut file = tokio::fs::File::create(&temp_path).await?;

    while let Some(chunk) = response.chunk().await? {
        if (opts.cancelled)() {
            drop(file);
            let _ = tokio::fs::remove_file(&temp_path).await;
            anyhow::bail!("已取消");
        }
        file.write_all(&chunk).await?;
        (opts.on_chunk)(chunk.len() as u64, total_size);
    }
    file.flush().await?;
    drop(file);

    let final_path = build_final_path(&temp_path, opts.dest_dir, fallback_name)?;
    if final_path.exists() {
        let _ = tokio::fs::remove_file(&temp_path).await;
        anyhow::bail!("同名文件已存在：{}", final_path.display());
    }
    tokio::fs::rename(&temp_path, &final_path).await?;

    Ok(final_path)
}

/// 隐藏临时文件路径：`.名字-<随机>.part`，点前缀不会被库扫描收录。
fn temp_path(dest_dir: &Path, fallback_name: &str) -> PathBuf {
    let unique: u64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 ^ (d.as_secs() << 8))
        .unwrap_or(0)
        ^ ((std::process::id() as u64) << 16);
    dest_dir.join(format!(
        ".{}.{unique:x}.part",
        sanitize_filename(&fallback_name.chars().take(40).collect::<String>())
    ))
}

/// 依据文件头魔数定扩展名，再读 tag 决定文件名。
/// 显式指定 FileType：temp 文件没有扩展名，lofty 自行探测会失败。
fn build_final_path(
    temp_path: &Path,
    dest_dir: &Path,
    fallback_name: &str,
) -> anyhow::Result<PathBuf> {
    use lofty::file::FileType;
    use std::io::Read;

    let mut magic = [0u8; 4];
    std::fs::File::open(temp_path)?.read_exact(&mut magic)?;
    let ext = match &magic {
        b"fLaC" => ("flac", FileType::Flac),
        m if &m[..3] == b"ID3" || (m[0] == 0xFF && m[1] & 0xE0 == 0xE0) => ("mp3", FileType::Mpeg),
        _ => anyhow::bail!("unrecognized audio format (magic: {magic:?})"),
    };

    let input = std::fs::File::open(temp_path)?;
    let tagged_file = lofty::probe::Probe::with_file_type(input, ext.1).read()?;
    let final_path = if let Some(tag) = tagged_file.primary_tag()
        && let (Some(title), Some(artist)) = (tag.title(), tag.artist())
    {
        dest_dir.join(format!("{title} - {artist}.{}", ext.0))
    } else {
        dest_dir.join(format!("{}.{}", sanitize_filename(fallback_name), ext.0))
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

    #[test]
    fn temp_path_是隐藏文件且在目标目录下() {
        let p = super::temp_path(std::path::Path::new("/tmp/music"), "晴天");
        assert!(p.starts_with("/tmp/music"));
        assert!(p.file_name().unwrap().to_str().unwrap().starts_with('.'));
        assert!(p.to_str().unwrap().ends_with(".part"));
    }
}
