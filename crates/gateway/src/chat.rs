//! Chat execution engine — re-exported from [`chelix_chat`] with the gateway
//! runtime adapter.

pub use chelix_chat::*;

use std::sync::Arc;

use {async_trait::async_trait, serde_json::Value};

use {chelix_channels::ChannelReplyTarget, chelix_tools::sandbox::SandboxRouter};

use crate::state::GatewayState;

// ── GatewayChatRuntime ──────────────────────────────────────────────────────

/// Adapts [`GatewayState`] to the [`ChatRuntime`] trait expected by
/// `chelix-chat`.
pub struct GatewayChatRuntime {
    state: Arc<GatewayState>,
}

impl GatewayChatRuntime {
    pub fn from_state(state: Arc<GatewayState>) -> Arc<dyn ChatRuntime> {
        Arc::new(Self { state })
    }
}

#[async_trait]
impl ChatRuntime for GatewayChatRuntime {
    // ── Broadcasting ────────────────────────────────────────────────────────

    async fn broadcast(&self, topic: &str, payload: Value) {
        crate::broadcast::broadcast(
            &self.state,
            topic,
            payload,
            crate::broadcast::BroadcastOpts::default(),
        )
        .await;
    }

    // ── Channel reply queue ─────────────────────────────────────────────────

    async fn push_channel_reply(&self, session_key: &str, target: ChannelReplyTarget) {
        self.state.push_channel_reply(session_key, target).await;
    }

    async fn drain_channel_replies(&self, session_key: &str) -> Vec<ChannelReplyTarget> {
        self.state.drain_channel_replies(session_key).await
    }

    async fn peek_channel_replies(&self, session_key: &str) -> Vec<ChannelReplyTarget> {
        self.state.peek_channel_replies(session_key).await
    }

    // ── Channel status log ──────────────────────────────────────────────────

    async fn push_channel_status_log(&self, session_key: &str, message: String) {
        self.state
            .push_channel_status_log(session_key, message)
            .await;
    }

    async fn drain_channel_status_log(&self, session_key: &str) -> Vec<String> {
        self.state.drain_channel_status_log(session_key).await
    }

    // ── Run error tracking ──────────────────────────────────────────────────

    async fn set_run_error(&self, run_id: &str, error: String) {
        self.state.set_run_error(run_id, error).await;
    }

    // ── Connection → session/project mapping ────────────────────────────────

    async fn active_session_key(&self, conn_id: &str) -> Option<String> {
        self.state
            .client_registry
            .read()
            .await
            .active_sessions
            .get(conn_id)
            .cloned()
    }

    async fn active_project_id(&self, conn_id: &str) -> Option<String> {
        self.state
            .client_registry
            .read()
            .await
            .active_projects
            .get(conn_id)
            .cloned()
    }

    // ── Immutable accessors ─────────────────────────────────────────────────

    fn hostname(&self) -> &str {
        &self.state.hostname
    }

    fn sandbox_router(&self) -> &Arc<SandboxRouter> {
        &self.state.sandbox_router
    }

    fn memory_manager(&self) -> Option<&chelix_memory::runtime::DynMemoryRuntime> {
        self.state.memory_manager.as_ref()
    }

    // ── Cached location ─────────────────────────────────────────────────────

    async fn cached_location(&self) -> Option<chelix_config::GeoLocation> {
        self.state.inner.read().await.cached_location.clone()
    }

    // ── TTS overrides ───────────────────────────────────────────────────────

    async fn tts_overrides(
        &self,
        session_key: &str,
        channel_key: &str,
    ) -> (Option<TtsOverride>, Option<TtsOverride>) {
        let inner = self.state.inner.read().await;
        let channel = inner
            .tts_channel_overrides
            .get(channel_key)
            .map(|o| TtsOverride {
                provider: o.provider.clone(),
                voice_id: o.voice_id.clone(),
                model: o.model.clone(),
            });
        let session = inner
            .tts_session_overrides
            .get(session_key)
            .map(|o| TtsOverride {
                provider: o.provider.clone(),
                voice_id: o.voice_id.clone(),
                model: o.model.clone(),
            });
        (channel, session)
    }

    // ── Services ────────────────────────────────────────────────────────────

    fn channel_outbound(&self) -> Option<Arc<dyn chelix_channels::ChannelOutbound>> {
        self.state.services.channel_outbound_arc()
    }

    fn channel_stream_outbound(&self) -> Option<Arc<dyn chelix_channels::ChannelStreamOutbound>> {
        self.state.services.channel_stream_outbound_arc()
    }

    fn tts_service(&self) -> &dyn chelix_service_traits::TtsService {
        &*self.state.services.tts
    }

    fn project_service(&self) -> &dyn chelix_service_traits::ProjectService {
        &*self.state.services.project
    }

    fn mcp_service(&self) -> &dyn chelix_service_traits::McpService {
        &*self.state.services.mcp
    }

    async fn chat_service(&self) -> Arc<dyn chelix_service_traits::ChatService> {
        self.state.chat()
    }

    async fn last_run_error(&self, run_id: &str) -> Option<String> {
        self.state.last_run_error(run_id).await
    }

    // ── Push notifications ──────────────────────────────────────────────────

    async fn send_push_notification(
        &self,
        title: &str,
        body: &str,
        url: Option<&str>,
        session_key: Option<&str>,
    ) -> error::Result<usize> {
        #[cfg(feature = "push-notifications")]
        {
            if let Some(push_service) = self.state.get_push_service().await {
                return crate::push::send_push_notification(
                    &push_service,
                    title,
                    body,
                    url,
                    session_key,
                )
                .await
                .map_err(|source| error::Error::message(source.to_string()));
            }
        }
        let _ = (title, body, url, session_key);
        Ok(0)
    }

    async fn take_steer_text(&self, session_key: &str) -> Option<Vec<String>> {
        self.state.take_steer_text(session_key).await
    }

    async fn is_fast_mode(&self, session_key: &str) -> bool {
        self.state.is_fast_mode(session_key).await
    }

    async fn trigger_auto_title(&self, session_key: &str) {
        let state = Arc::clone(&self.state);
        let key = session_key.to_string();
        tokio::spawn(async move {
            crate::session::title::generate_title_if_needed(&state, &key).await;
        });
    }
}
