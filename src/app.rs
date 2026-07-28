use clap::Parser;
use dialoguer::MultiSelect;

#[derive(Debug, Parser)]
#[command(name = "rmus-dl", about = "Music Downloader Written in Rust")]
pub struct Cli {
    pub keyword: String,
}

use crate::{download, twot58::TwoT58};

pub async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let keyword = cli.keyword;
    let service = TwoT58::try_new()?;
    println!("Searching for \"{}\"...", keyword);
    let results = service.search(&keyword).await?;

    let titles = results
        .iter()
        .map(|r| r.title.as_str())
        .collect::<Vec<&str>>();

    let multi_selection = MultiSelect::new()
        .with_prompt("Which songs do you want to download?")
        .items(titles)
        .interact()?;

    for selection in multi_selection {
        let download_url = service.get_download_url(&results[selection]).await?;

        println!(
            "Downloading {} from \"{}\"...",
            results[selection].title, download_url
        );
        if let Ok(file_path) = download::download_file(download_url).await {
            println!("Downloaded to \"{}\"", file_path.canonicalize()?.display());

            println!("Fetching metadata for \"{}\"...", results[selection].title);
            if let Err(e) = service
                .write_metadata(&file_path, &results[selection])
                .await
            {
                eprintln!("Failed to write metadata: {}", e);
            } else {
                println!("Metadata written successfully!");
            }
        } else {
            println!(
                "Failed to download \"{}\". Skipping...",
                results[selection].title
            );
        }
    }

    Ok(())
}
