#[tokio::main]
async fn main() -> anyhow::Result<()> {
    rmus_dl::app::run().await?;
    Ok(())
}
