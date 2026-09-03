use serde::{Deserialize, Serialize};

/// Session key: agent:<id>:main or agent:<id>:channel:<ch>:account:<acct>:peer:<kind>:<id>
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionKey(pub String);

/// DM scope mode for session key generation.
#[derive(Debug, Clone)]
pub enum DmScope {
    /// All DMs collapse into a single session.
    Main,
    /// Each peer gets a separate session.
    PerPeer,
    /// Each channel+peer gets a separate session.
    PerChannelPeer,
    /// Full isolation: account+channel+peer.
    PerAccountChannelPeer,
}

impl SessionKey {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn main(agent_id: &str) -> Self {
        Self(format!("agent:{agent_id}:main"))
    }

    pub fn for_peer(
        agent_id: &str,
        channel: &str,
        account: &str,
        peer_kind: &str,
        peer_id: &str,
    ) -> Self {
        Self(format!(
            "agent:{agent_id}:channel:{channel}:account:{account}:peer:{peer_kind}:{peer_id}"
        ))
    }
}

impl AsRef<str> for SessionKey {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::fmt::Display for SessionKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl From<String> for SessionKey {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<&str> for SessionKey {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}
