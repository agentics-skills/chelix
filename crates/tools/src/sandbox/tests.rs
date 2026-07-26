#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

#[cfg(target_os = "macos")]
use super::apple::*;
use {
    super::{containers::*, docker::*, router::*, types::*},
    crate::{
        command::{CommandOptions, CommandOutput},
        error::{Error, Result},
    },
};

struct TestSandbox {
    backend: SandboxBackendId,
    ensure_ready_error: Option<String>,
    command_error: Option<String>,
    ensure_ready_calls: AtomicUsize,
    command_calls: AtomicUsize,
    cleanup_calls: AtomicUsize,
}

impl TestSandbox {
    fn new(
        backend: SandboxBackendId,
        ensure_ready_error: Option<&str>,
        command_error: Option<&str>,
    ) -> Self {
        Self {
            backend,
            ensure_ready_error: ensure_ready_error.map(ToOwned::to_owned),
            command_error: command_error.map(ToOwned::to_owned),
            ensure_ready_calls: AtomicUsize::new(0),
            command_calls: AtomicUsize::new(0),
            cleanup_calls: AtomicUsize::new(0),
        }
    }

    #[cfg(target_os = "macos")]
    fn ensure_ready_calls(&self) -> usize {
        self.ensure_ready_calls.load(Ordering::SeqCst)
    }

    #[cfg(target_os = "macos")]
    fn command_calls(&self) -> usize {
        self.command_calls.load(Ordering::SeqCst)
    }
}

#[test]
fn truncate_output_for_display_handles_multibyte_boundary() {
    let mut output = format!("{}л{}", "a".repeat(1999), "z".repeat(10));
    truncate_output_for_display(&mut output, 2000);
    assert!(output.contains("[output truncated]"));
    assert!(!output.contains('л'));
}

#[async_trait::async_trait]
impl Sandbox for TestSandbox {
    fn backend_id(&self) -> SandboxBackendId {
        self.backend
    }

    fn provides_fs_isolation(&self) -> bool {
        true
    }

    async fn ensure_ready(&self, _id: &SandboxId) -> Result<()> {
        self.ensure_ready_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(ref msg) = self.ensure_ready_error {
            return Err(Error::message(msg));
        }
        Ok(())
    }

    async fn run_command(
        &self,
        _id: &SandboxId,
        _command: &str,
        _opts: &CommandOptions,
    ) -> Result<CommandOutput> {
        self.command_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(ref msg) = self.command_error {
            return Err(Error::message(msg));
        }
        Ok(CommandOutput {
            stdout: "ok".into(),
            stderr: String::new(),
            exit_code: 0,
        })
    }

    async fn cleanup(&self, _id: &SandboxId) -> Result<()> {
        self.cleanup_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod apple;
mod core;
mod docker_router;
mod network;
mod resolve_env;
mod selection;
