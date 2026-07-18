use std::{io::Write as _, net::SocketAddr};

use {
    anyhow::{Context, Result},
    chelix_protocol::{TOOLS_SERVICE_PROTOCOL_VERSION, TOOLS_SERVICE_TOKEN_ENV, ToolsServiceReady},
    clap::Parser,
    tokio::io::AsyncReadExt,
    uuid::Uuid,
};

#[derive(Debug, Parser)]
#[command(about = "Chelix managed filesystem tools service")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:0")]
    listen: SocketAddr,
    #[arg(long, env = TOOLS_SERVICE_TOKEN_ENV)]
    token: Option<String>,
    #[arg(long)]
    shutdown_on_stdin_eof: bool,
}

pub async fn run() -> Result<()> {
    let args = Args::parse();
    let token = args.token.unwrap_or_else(generate_token);
    let listener = tokio::net::TcpListener::bind(args.listen)
        .await
        .context("binding tools service")?;
    let port = listener
        .local_addr()
        .context("reading tools service address")?
        .port();
    write_ready(&ToolsServiceReady {
        protocol_version: TOOLS_SERVICE_PROTOCOL_VERSION,
        port,
        token: token.clone(),
    })?;

    let shutdown_on_stdin_eof = args.shutdown_on_stdin_eof;
    axum::serve(listener, crate::api::router(token))
        .with_graceful_shutdown(async move {
            if shutdown_on_stdin_eof {
                parent_closed_stdin().await;
            } else {
                std::future::pending::<()>().await;
            }
        })
        .await
        .context("serving tools API")
}

fn generate_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

fn write_ready(ready: &ToolsServiceReady) -> Result<()> {
    let json = serde_json::to_string(ready).context("encoding startup message")?;
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    writeln!(output, "{json}").context("writing startup message")?;
    output.flush().context("flushing startup message")
}

async fn parent_closed_stdin() {
    let mut input = tokio::io::stdin();
    let mut buffer = [0_u8; 1024];
    loop {
        match input.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {},
        }
    }
}
