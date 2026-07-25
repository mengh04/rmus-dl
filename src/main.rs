use clap::Parser;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = rmus_dl::cli::Cli::parse();

    rmus_dl::app::run(cli).await?;

    Ok(())
}
