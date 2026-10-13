use clap::Parser;
use dialoguer::MultiSelect;

use crate::download;
use crate::sources::{MusicSource, kugou::Kugou, kuwo::Kuwo, write_metadata};

#[derive(Debug, Parser)]
#[command(name = "rmus-dl", about = "Music Downloader Written in Rust")]
pub struct Cli {
    /// 搜索关键词，多个参数自动用空格拼接
    pub keyword: Vec<String>,
}

pub async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let keyword = cli.keyword.join(" ");
    if keyword.is_empty() {
        anyhow::bail!("usage: rmus-dl <keyword>");
    }

    let kuwo = Kuwo::new()?;
    let kugou = Kugou::new()?;

    println!("Searching for \"{keyword}\"...");
    let (kw_results, kg_results) = tokio::join!(kuwo.search(&keyword), kugou.search(&keyword));

    let mut results = Vec::new();
    match kw_results {
        Ok(r) => results.extend(r),
        Err(e) => eprintln!("kuwo search failed: {e}"),
    }
    match kg_results {
        Ok(r) => results.extend(r),
        Err(e) => eprintln!("kugou search failed: {e}"),
    }
    if results.is_empty() {
        anyhow::bail!("No results found for \"{keyword}\"");
    }

    let titles = results
        .iter()
        .map(|r| match &r.artist {
            Some(artist) => format!("[{}] {} - {}", r.source, r.title, artist),
            None => format!("[{}] {}", r.source, r.title),
        })
        .collect::<Vec<String>>();

    let multi_selection = MultiSelect::new()
        .with_prompt("Which songs do you want to download?")
        .items(&titles)
        .interact()?;

    let mut failures: Vec<String> = Vec::new();

    for selection in multi_selection {
        let result = &results[selection];
        let (source_name, download_url) = match result.source {
            "kuwo" => ("kuwo", kuwo.get_download_url(result).await),
            "kugou" => ("kugou", kugou.get_download_url(result).await),
            other => anyhow::bail!("unknown source: {other}"),
        };
        let download_url = match download_url {
            Ok(url) => url,
            Err(e) => {
                eprintln!("Failed to get download url for \"{}\": {e}", result.title);
                failures.push(result.title.clone());
                continue;
            }
        };

        println!("Downloading \"{}\" via {source_name}...", result.title);
        let fallback_name = match &result.artist {
            Some(artist) => format!("{} - {}", result.title, artist),
            None => result.title.clone(),
        };
        let file_path = match download::download_file(download_url, &fallback_name).await {
            Ok(path) => path,
            Err(e) => {
                eprintln!("Failed to download \"{}\": {e}", result.title);
                failures.push(result.title.clone());
                continue;
            }
        };

        // 元数据是门禁：拿不到或写不进去，这次下载就算失败，清理产物
        let metadata = match result.source {
            "kuwo" => kuwo.get_metadata(result).await,
            "kugou" => kugou.get_metadata(result).await,
            other => anyhow::bail!("unknown source: {other}"),
        };
        let meta_result = match metadata {
            Ok(meta) => write_metadata(&file_path, &meta).await,
            Err(e) => Err(e),
        };
        match meta_result {
            Ok(()) => println!(
                "Downloaded \"{}\" with metadata -> \"{}\"",
                result.title,
                file_path.canonicalize()?.display()
            ),
            Err(e) => {
                eprintln!(
                    "Metadata gate failed for \"{}\" ({e}), removing \"{}\"",
                    result.title,
                    file_path.display()
                );
                if let Err(rm_err) = tokio::fs::remove_file(&file_path).await {
                    eprintln!("Failed to remove incomplete download: {rm_err}");
                }
                failures.push(result.title.clone());
            }
        }
    }

    if !failures.is_empty() {
        anyhow::bail!(
            "{} download(s) failed the metadata gate: {}",
            failures.len(),
            failures.join(", ")
        );
    }

    Ok(())
}
