use std::{io::Write as _, net::SocketAddr, path::PathBuf, sync::Arc};

use {
    anyhow::{Context, Result, anyhow},
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
    #[arg(long)]
    working_dir: PathBuf,
}

pub async fn run() -> Result<()> {
    let args = Args::parse();
    crate::rmux::verify_runtime(&args.working_dir).await?;
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
    let terminal_manager = Arc::new(
        crate::terminal::TerminalManager::new(args.working_dir)
            .context("initializing terminal manager")?,
    );
    let serve_result = axum::serve(
        listener,
        crate::api::router(token, Arc::clone(&terminal_manager)),
    )
    .with_graceful_shutdown(async move {
        if shutdown_on_stdin_eof {
            parent_closed_stdin().await;
        } else {
            std::future::pending::<()>().await;
        }
    })
    .await
    .context("serving tools API");
    let terminal_shutdown_result = terminal_manager
        .shutdown()
        .await
        .context("shutting down terminal manager");
    finish_service(serve_result, terminal_shutdown_result)
}

fn finish_service(serve_result: Result<()>, terminal_shutdown_result: Result<()>) -> Result<()> {
    let mut errors = Vec::new();
    if let Err(error) = serve_result {
        errors.push(("tools API", error));
    }
    if let Err(error) = terminal_shutdown_result {
        errors.push(("terminal manager shutdown", error));
    }
    if errors.is_empty() {
        return Ok(());
    }
    if errors.len() == 1 {
        return match errors.pop() {
            Some((_, error)) => Err(error),
            None => Err(anyhow!("service shutdown failed without an error")),
        };
    }
    Err(anyhow!(
        errors
            .into_iter()
            .map(|(stage, error)| format!("{stage} failed: {error:#}"))
            .collect::<Vec<_>>()
            .join("; ")
    ))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finish_service_propagates_each_error() {
        let serve_error = match finish_service(Err(anyhow!("serve failed")), Ok(())) {
            Ok(()) => panic!("expected serve error"),
            Err(error) => error,
        };
        assert_eq!(serve_error.to_string(), "serve failed");
        let shutdown_error = match finish_service(Ok(()), Err(anyhow!("shutdown failed"))) {
            Ok(()) => panic!("expected shutdown error"),
            Err(error) => error,
        };
        assert_eq!(shutdown_error.to_string(), "shutdown failed");
    }

    #[test]
    fn finish_service_preserves_serve_and_shutdown_errors() {
        let error = match finish_service(
            Err(anyhow!("serve failed")),
            Err(anyhow!("terminal shutdown failed")),
        ) {
            Ok(()) => panic!("expected combined service error"),
            Err(error) => error.to_string(),
        };

        assert!(error.contains("serve failed"));
        assert!(error.contains("terminal shutdown failed"));
    }
}
