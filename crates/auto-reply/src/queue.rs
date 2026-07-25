/// Per-session followup queue for inbound messages.
///
/// Modes: per-message, batch, debounce.
/// Drop policies: oldest, newest, none.

#[derive(Debug, Clone)]
pub enum QueueMode {
    /// Each inbound message triggers a separate agent run.
    PerMessage,
    /// Accumulate multiple inbound messages into a single agent run.
    Batch,
    /// Wait for an idle period before invoking the agent.
    Debounce { idle_ms: u64 },
}

#[derive(Debug, Clone)]
pub enum DropPolicy {
    Oldest,
    Newest,
    None,
}
