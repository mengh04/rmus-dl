use dialoguer::Select;

use crate::{cli::Cli, download, twot58::TwoT58};

pub async fn run(cli: Cli) -> anyhow::Result<()> {
    let keyword = cli.keyword;
    let service = TwoT58::try_new()?;
    let results = service.search(&keyword).await?;

    let titles = results
        .iter()
        .map(|r| r.title.as_str())
        .collect::<Vec<&str>>();

    let selection = Select::new()
        .with_prompt("Which song do you want to download?")
        .items(titles)
        .interact()?;

    let download_url = service.get_download_url(&results[selection]).await?;

    println!("Downloading...");
    let file_path = download::download_file(download_url).await?;
    println!("Downloaded to \"{}\"", file_path.canonicalize()?.display());

    println!("Writing metadata...");
    service
        .write_metadata(&file_path, &results[selection])
        .await?;
    println!("Metadata written successfully!");

    Ok(())
}
