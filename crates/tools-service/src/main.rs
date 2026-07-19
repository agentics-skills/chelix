mod api;
mod app;
mod list_directory;
mod ripgrep;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    app::run().await
}
