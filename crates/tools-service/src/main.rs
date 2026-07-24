mod api;
mod app;
mod interactive_terminal;
mod list_directory;
mod process;
mod ripgrep;
mod rmux;
mod terminal;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    app::run().await
}
