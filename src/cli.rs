use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "rmus-dl", about = "Music Downloader Written in Rust")]
pub struct Cli {
    pub keyword: String,
}
