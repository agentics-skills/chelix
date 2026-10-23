use crate::Procedure;

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct StopSessionError {
    pub message: String,
}

/// Stop one session's current activity, or one run by its id.
pub enum StopSession {
    /// Stop the current activity of `key`.
    ///
    /// `run_id: None` stops whatever is current. `Some` stops that run only when
    /// it is still the current run of the session.
    Session { key: String, run_id: Option<String> },
    /// Cancel one run token. The session key is not resolved.
    Run { run_id: String },
}

/// Result of a successful `StopSession` call.
pub struct StopSessionOutcome {
    pub cancelled: bool,
    pub run_id: Option<String>,
}

impl Procedure for StopSession {
    type Error = StopSessionError;
    type Output = StopSessionOutcome;
}
