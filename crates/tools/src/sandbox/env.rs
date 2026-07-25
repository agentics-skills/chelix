//! Resolved execution environment for filesystem and process tools.

use std::sync::Arc;

use super::{Sandbox, SandboxId};

/// The single resolved environment for a tool execution.
pub enum ExecEnv {
    /// Explicit host execution with sandboxing disabled by policy.
    Host,
    /// A prepared backend that provides filesystem isolation.
    Sandbox {
        backend: Arc<dyn Sandbox>,
        id: SandboxId,
    },
}
