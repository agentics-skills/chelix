//! Tool implementations and policy enforcement.
//!
//! Tools: command execution, browser, canvas, message, cron, sessions,
//! memory, image gen, plus channel and plugin tools.
//!
//! Policy: multi-layered allow/deny (global, per-agent, per-provider,
//! per-group, per-sender, sandbox).

pub mod approval;
pub mod branch_session;
pub mod browser;
mod client;
pub mod command;
#[cfg(test)]
pub mod contract;
pub mod cron_tool;
pub mod error;
pub mod file_io;
#[cfg(feature = "firecrawl")]
pub mod firecrawl;
#[cfg(feature = "fs-tools")]
pub mod fs;
pub mod image_cache;
#[cfg(feature = "provider-openai-codex")]
pub mod image_generation;
pub mod list_directory;
pub mod location;
pub mod map;
pub mod params;
pub mod policy;
pub mod process;
pub mod ripgrep;
pub mod sandbox;
pub mod sandbox_packages;
pub mod send_document;
pub mod send_image;
pub mod session_model_override;
pub mod session_state;
pub mod sessions_communicate;
pub mod sessions_manage;
pub mod skill_tools;
pub mod spawn_agent;
pub mod spawn_agent_tasks;
pub mod task_list;
pub mod tmux_command;
pub mod tools_service;
pub mod webhook_tool;

pub use {
    client::{build_http_client, init_shared_http_client, shared_http_client},
    error::{Error, Result},
};
