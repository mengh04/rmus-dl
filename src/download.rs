use std::{path::PathBuf, time::Duration};
use tokio::io::AsyncWriteExt;

use indicatif::{ProgressBar, ProgressStyle};
use lofty::{file::TaggedFileExt, probe::Probe, tag::Accessor};

pub async fn download_file(url: reqwest::Url) -> anyhow::Result<PathBuf> {
    let mut response = reqwest::get(url).await?.error_for_status()?;

    let total_size = response.content_length();

    let progress = total_size
        .map(ProgressBar::new)
        .unwrap_or_else(ProgressBar::new_spinner);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("{bar:40.cyan/blue} {bytes}/{total_bytes} ({eta})")
            .unwrap()
            .progress_chars("=>-"),
    );
    progress.enable_steady_tick(Duration::from_millis(100));

    let temp_path = PathBuf::from("temp.flac.part");

    let mut file = tokio::fs::File::create(&temp_path).await?;
    while let Some(chunk) = response.chunk().await? {
        file.write_all(&chunk).await?;
        progress.inc(chunk.len() as u64);
    }

    file.flush().await?;
    drop(file);

    progress.finish_with_message("Download complete");

    let input = std::fs::File::open(&temp_path)?;

    let tagged_file = Probe::with_file_type(input, lofty::file::FileType::Flac).read()?;

    let final_path = if let Some(tag) = tagged_file.primary_tag()
        && let (Some(title), Some(artist)) = (tag.title(), tag.artist())
    {
        PathBuf::from(format!("{} - {}.flac", title, artist))
    } else {
        anyhow::bail!("No primary tag found or missing title/artist in the FLAC file");
    };
    drop(tagged_file);

    tokio::fs::rename(&temp_path, &final_path).await?;

    Ok(final_path)
}
