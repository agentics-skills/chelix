use {chelix_call_bus::Procedure, chelix_sessions::SessionKey};

use crate::error::ServiceError;

/// Stop one session's current activity, or one run by its id.
pub enum StopSession {
    /// Stop the current activity of `key`.
    ///
    /// `run_id: None` stops whatever is current. `Some` stops that run only when
    /// it is still the current local or external run of the session.
    Session {
        key: SessionKey,
        run_id: Option<String>,
    },
    /// Cancel one local run token. The session key is not resolved.
    Run { run_id: String },
}

/// Result of a successful `StopSession` call.
pub struct StopSessionOutcome {
    pub cancelled: bool,
    pub run_id: Option<String>,
}

impl Procedure for StopSession {
    type Error = ServiceError;
    type Output = StopSessionOutcome;
}
