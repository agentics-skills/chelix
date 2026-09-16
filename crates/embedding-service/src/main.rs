mod app;
mod engine;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    app::run().await
}
