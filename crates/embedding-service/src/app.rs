use std::{io::Write as _, net::Ipv4Addr, path::PathBuf, sync::Arc};

use {
    anyhow::{Context, Result},
    chelix_embedding_service::{EmbeddingEngine, api},
    chelix_protocol::{EMBEDDING_SERVICE_PROTOCOL_VERSION, EmbeddingServiceReady},
    clap::Parser,
    tokio::io::AsyncReadExt,
};

use crate::engine::LocalGgufEngine;

#[derive(Debug, Parser)]
#[command(about = "Chelix local GGUF embedding service")]
struct Args {
    #[arg(long)]
    model: PathBuf,
}

pub(crate) async fn run() -> Result<()> {
    init_tracing();
    let args = Args::parse();
    let engine = Arc::new(LocalGgufEngine::new(args.model)?);
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .context("binding local embedding service")?;
    let port = listener
        .local_addr()
        .context("reading local embedding service address")?
        .port();
    let ready = EmbeddingServiceReady {
        protocol_version: EMBEDDING_SERVICE_PROTOCOL_VERSION,
        port,
        model: engine.metadata().clone(),
    };
    write_ready(&ready)?;

    axum::serve(listener, api::router(engine))
        .with_graceful_shutdown(parent_closed_stdin())
        .await
        .context("serving local embedding API")
}

fn write_ready(ready: &EmbeddingServiceReady) -> Result<()> {
    let json = serde_json::to_string(ready).context("encoding startup message")?;
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    writeln!(output, "{json}").context("writing startup message")?;
    output.flush().context("flushing startup message")
}

async fn parent_closed_stdin() {
    let mut stdin = tokio::io::stdin();
    let mut byte = [0_u8; 1];
    loop {
        match stdin.read(&mut byte).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {},
        }
    }
}

#[cfg(feature = "tracing")]
fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
}

#[cfg(not(feature = "tracing"))]
fn init_tracing() {}
