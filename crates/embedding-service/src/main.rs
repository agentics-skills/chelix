mod app;
#[allow(unsafe_code)]
mod engine;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    app::run().await
}
