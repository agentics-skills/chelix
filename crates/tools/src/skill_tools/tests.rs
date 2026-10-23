#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use {
    chelix_agents::{tool_context::ToolExecutionContext, tool_registry::AgentTool},
    serde_json::json,
};

use super::*;

fn test_context() -> ToolExecutionContext {
    ToolExecutionContext::for_session_with_agent(
        chelix_sessions::SessionKey::new("test"),
        "test-agent",
    )
}

#[path = "crud_write.rs"]
mod crud_write;
#[path = "read.rs"]
mod read;
