mod api;
mod app;
mod edit_file;
mod interactive_terminal;
mod list_directory;
mod overwrite_file;
mod process;
mod read_file;
mod read_media;
mod ripgrep;
mod rmux;
mod terminal;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    app::run().await
}
