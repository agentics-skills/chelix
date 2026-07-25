/// Session key: agent:<id>:main or agent:<id>:channel:<ch>:account:<acct>:peer:<kind>:<id>
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
