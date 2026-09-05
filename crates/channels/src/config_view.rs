use {
    crate::gating::{DmPolicy, GroupPolicy},
    chelix_common::ConfigModelOverride,
};

/// Typed read-only view of common channel account config fields.
///
/// Each plugin's concrete config type (e.g. `TelegramAccountConfig`) implements
/// this trait. The gateway and registry access typed fields instead of digging
/// into raw `serde_json::Value`.
///
/// The store persists `Value` via `StoredChannel`. This trait is purely for
/// typed read access to shared config fields.
pub trait ChannelConfigView: Send + Sync + std::fmt::Debug {
    /// DM user/peer allowlist.
    fn allowlist(&self) -> &[String];

    /// Group/chat ID allowlist.
    fn group_allowlist(&self) -> &[String];

    /// DM access policy.
    fn dm_policy(&self) -> DmPolicy;

    /// Group access policy.
    fn group_policy(&self) -> GroupPolicy;

    /// Default model/reasoning override for this channel account.
    fn model_override(&self) -> Option<&ConfigModelOverride>;

    /// Provider name associated with the model.
    fn model_provider(&self) -> Option<&str>;

    /// Default agent id for this channel account.
    fn agent_id(&self) -> Option<&str> {
        None
    }

    // ── Per-channel / per-user override methods ─────────────────────────────

    /// Model/reasoning override for a specific channel/chat ID.
    fn channel_model_override(&self, _channel_id: &str) -> Option<&ConfigModelOverride> {
        None
    }

    /// Provider override for a specific channel/chat ID.
    fn channel_model_provider(&self, _channel_id: &str) -> Option<&str> {
        None
    }

    /// Model/reasoning override for a specific user.
    fn user_model_override(&self, _user_id: &str) -> Option<&ConfigModelOverride> {
        None
    }

    /// Provider override for a specific user.
    fn user_model_provider(&self, _user_id: &str) -> Option<&str> {
        None
    }

    /// Agent override for a specific channel/chat ID.
    fn channel_agent_id(&self, _channel_id: &str) -> Option<&str> {
        None
    }

    /// Agent override for a specific user.
    fn user_agent_id(&self, _user_id: &str) -> Option<&str> {
        None
    }

    /// Resolve effective model/reasoning override: user > channel > account default.
    fn resolve_model_override(
        &self,
        channel_id: &str,
        user_id: &str,
    ) -> Option<&ConfigModelOverride> {
        self.user_model_override(user_id)
            .or_else(|| self.channel_model_override(channel_id))
            .or_else(|| self.model_override())
    }

    /// Resolve effective provider: user > channel > account default.
    fn resolve_model_provider(&self, channel_id: &str, user_id: &str) -> Option<&str> {
        self.user_model_provider(user_id)
            .or_else(|| self.channel_model_provider(channel_id))
            .or_else(|| self.model_provider())
    }

    /// Resolve effective agent id: user > channel > account default.
    fn resolve_agent_id(&self, channel_id: &str, user_id: &str) -> Option<&str> {
        self.user_agent_id(user_id)
            .or_else(|| self.channel_agent_id(channel_id))
            .or_else(|| self.agent_id())
    }
}
