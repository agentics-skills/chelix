use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc};

use tracing::{debug, warn};

use {
    chelix_protocol::{ErrorShape, ResponseFrame, error_codes},
    chelix_sessions::SessionKey,
};

use crate::state::GatewayState;

// ── Types ────────────────────────────────────────────────────────────────────

/// Transport-level session semantics for an RPC method invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MethodTransport {
    StatefulConnection,
    StatelessHttp { session_id: Option<SessionKey> },
}

impl MethodTransport {
    #[must_use]
    pub fn is_stateless_http(&self) -> bool {
        matches!(self, Self::StatelessHttp { .. })
    }
}

/// Context passed to every method handler.
pub struct MethodContext {
    pub request_id: String,
    pub method: String,
    pub params: serde_json::Value,
    pub client_conn_id: String,
    pub transport: MethodTransport,
    pub client_role: String,
    pub client_scopes: Vec<String>,
    pub state: Arc<GatewayState>,
    /// Optional channel context from the request frame (v4).
    pub channel: Option<String>,
}

impl MethodContext {
    /// Resolve the session from the current transport without crossing transport modes.
    pub async fn resolved_session_id(&self) -> Option<SessionKey> {
        match &self.transport {
            MethodTransport::StatefulConnection => self
                .state
                .client_registry
                .read()
                .await
                .active_sessions
                .get(&self.client_conn_id)
                .cloned()
                .map(SessionKey::new),
            MethodTransport::StatelessHttp { session_id } => session_id.clone(),
        }
    }
}

/// The result a method handler produces.
pub type MethodResult = Result<serde_json::Value, ErrorShape>;

/// A boxed async method handler.
pub type HandlerFn =
    Box<dyn Fn(MethodContext) -> Pin<Box<dyn Future<Output = MethodResult> + Send>> + Send + Sync>;

// ── Scope authorization ──────────────────────────────────────────────────────

const READ_METHODS: &[&str] = &[
    "health",
    "logs.tail",
    "logs.list",
    "logs.status",
    "channels.status",
    "channels.list",
    "channels.senders.list",
    "status",
    "usage.status",
    "usage.cost",
    "tts.status",
    "tts.providers",
    "stt.status",
    "stt.providers",
    "models.list",
    "models.list_all",
    #[cfg(feature = "agent")]
    "agents.list",
    #[cfg(feature = "agent")]
    "agents.get",
    #[cfg(feature = "agent")]
    "agents.files.list",
    #[cfg(feature = "agent")]
    "agents.files.get",
    "user.get",
    "skills.list",
    "skills.status",
    "skills.security.status",
    "skills.security.scan",
    "skills.repos.list",
    "skills.recipe",
    "skills.bundled.categories",
    "voicewake.get",
    "sessions.list",
    "sessions.preview",
    "sessions.history.subscribe",
    "sessions.search",
    "sessions.branches",
    "sessions.run_detail",
    "sessions.share.list",
    "projects.list",
    "projects.get",
    "projects.context",
    "projects.complete_path",
    "cron.list",
    "cron.status",
    "cron.runs",
    "webhooks.list",
    "webhooks.profiles",
    "webhooks.deliveries",
    "webhooks.delivery.get",
    "webhooks.delivery.payload",
    "webhooks.delivery.actions",
    "heartbeat.status",
    "heartbeat.runs",
    "system-presence",
    "last-heartbeat",
    "chat.history",
    "chat.context",
    "chat.raw_prompt",
    "chat.full_context",
    "chat.queued_prompts.status",
    "providers.available",
    "mcp.list",
    "mcp.status",
    "mcp.tools",
    "mcp.config.get",
    "voice.config.get",
    "voice.config.voxtral_requirements",
    "voice.providers.all",
    "voice.elevenlabs.catalog",
    "voice.personas.list",
    "voice.personas.get",
    "memory.status",
    "memory.config.get",
    "memory.qmd.status",
    "hooks.list",
    "external_agents.list",
    "external_agents.status",
    "system.describe",
    "voicecall.status",
    #[cfg(feature = "telephony")]
    "phone.providers.all",
];

const WRITE_METHODS: &[&str] = &[
    "a2ui.action",
    "send",
    "chat.prompt_memory.refresh",
    "agent",
    "agent.wait",
    "user.update",
    #[cfg(feature = "agent")]
    "agents.create",
    #[cfg(feature = "agent")]
    "agents.update",
    #[cfg(feature = "agent")]
    "agents.delete",
    #[cfg(feature = "agent")]
    "agents.set_default",
    #[cfg(feature = "agent")]
    "agents.set_session",
    #[cfg(feature = "agent")]
    "agents.files.set",
    "wake",
    "talk.mode",
    "tts.enable",
    "tts.disable",
    "tts.convert",
    "tts.setProvider",
    "stt.transcribe",
    "stt.setProvider",
    "voicewake.set",
    "chat.send",
    "chat.send_sync",
    "chat.abort",
    "chat.queued_prompts.remove",
    "external_agents.bind",
    "external_agents.unbind",
    "chat.clear",
    "chat.compact",
    "browser.request",
    "logs.ack",
    "providers.save_key",
    "providers.set_model_preferences",
    "providers.remove_key",
    "channels.add",
    "channels.remove",
    "channels.update",
    "channels.retry_ownership",
    "channels.senders.approve",
    "channels.senders.deny",
    "sessions.switch",
    "sessions.fork",
    "sessions.truncate_tail",
    "sessions.voice.generate",
    "sessions.clear_all",
    "sessions.share.create",
    "sessions.share.revoke",
    "projects.upsert",
    "projects.delete",
    "projects.detect",
    "skills.install",
    "skills.remove",
    "skills.repos.remove",
    "skills.repos.export",
    "skills.repos.import",
    "skills.repos.unquarantine",
    "skills.emergency_disable",
    "skills.skill.trust",
    "skills.skill.enable",
    "skills.skill.disable",
    "skills.skill.save",
    "skills.bundled.toggle_category",
    "mcp.add",
    "mcp.remove",
    "mcp.enable",
    "mcp.disable",
    "mcp.restart",
    "mcp.reauth",
    "mcp.update",
    "mcp.config.update",
    "mcp.oauth.start",
    "mcp.oauth.complete",
    "cron.add",
    "cron.update",
    "cron.remove",
    "cron.run",
    "webhooks.get",
    "webhooks.create",
    "webhooks.update",
    "webhooks.delete",
    "heartbeat.update",
    "heartbeat.run",
    "voice.config.save_key",
    "voice.config.save_settings",
    "voice.config.remove_key",
    "voice.provider.toggle",
    "voice.override.session.set",
    "voice.override.session.clear",
    "voice.override.channel.set",
    "voice.override.channel.clear",
    "voice.personas.create",
    "voice.personas.update",
    "voice.personas.delete",
    "voice.personas.set_active",
    "memory.config.update",
    "hooks.enable",
    "hooks.disable",
    "hooks.save",
    "hooks.reload",
    "location.result",
    "voicecall.initiate",
    "voicecall.end",
    #[cfg(feature = "telephony")]
    "phone.provider.toggle",
    #[cfg(feature = "telephony")]
    "phone.config.save_key",
    #[cfg(feature = "telephony")]
    "phone.config.save_settings",
    #[cfg(feature = "telephony")]
    "phone.config.remove_key",
    "subscribe",
    "unsubscribe",
    "channel.join",
    "channel.leave",
];

const APPROVAL_METHODS: &[&str] = &["command.approval.request", "command.approval.resolve"];

fn is_in(method: &str, list: &[&str]) -> bool {
    list.contains(&method)
}

/// Check role + scopes for a method. Returns None if authorized, Some(error) if not.
pub fn authorize_method(method: &str, role: &str, scopes: &[String]) -> Option<ErrorShape> {
    use chelix_protocol::scopes as s;

    if role != "operator" {
        return Some(ErrorShape::new(
            error_codes::FORBIDDEN,
            format!("unauthorized role: {role}"),
        ));
    }

    let has = |scope: &str| scopes.iter().any(|s| s == scope);
    if has(s::ADMIN) {
        return None;
    }

    if is_in(method, APPROVAL_METHODS) && !has(s::APPROVALS) {
        return Some(ErrorShape::new(
            error_codes::UNAUTHORIZED,
            "missing scope: operator.approvals",
        ));
    }
    if is_in(method, READ_METHODS) && !(has(s::READ) || has(s::WRITE)) {
        return Some(ErrorShape::new(
            error_codes::UNAUTHORIZED,
            "missing scope: operator.read",
        ));
    }
    if is_in(method, WRITE_METHODS) && !has(s::WRITE) {
        return Some(ErrorShape::new(
            error_codes::UNAUTHORIZED,
            "missing scope: operator.write",
        ));
    }

    if is_in(method, APPROVAL_METHODS)
        || is_in(method, READ_METHODS)
        || is_in(method, WRITE_METHODS)
    {
        return None;
    }

    Some(ErrorShape::new(
        error_codes::UNAUTHORIZED,
        "missing scope: operator.admin",
    ))
}

// ── Method registry ──────────────────────────────────────────────────────────

pub struct MethodRegistry {
    handlers: HashMap<String, HandlerFn>,
}

impl Default for MethodRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl MethodRegistry {
    pub fn new() -> Self {
        let mut reg = Self {
            handlers: HashMap::new(),
        };
        reg.register_defaults();
        reg
    }

    pub fn register(&mut self, method: impl Into<String>, handler: HandlerFn) {
        self.handlers.insert(method.into(), handler);
    }

    pub async fn dispatch(&self, ctx: MethodContext) -> ResponseFrame {
        let method = ctx.method.clone();
        let request_id = ctx.request_id.clone();
        let conn_id = ctx.client_conn_id.clone();

        if let Some(err) = authorize_method(&method, &ctx.client_role, &ctx.client_scopes) {
            warn!(method, conn_id = %conn_id, code = %err.code, "method auth denied");
            return ResponseFrame::err(&request_id, err);
        }

        let Some(handler) = self.handlers.get(&method) else {
            warn!(method, conn_id = %conn_id, "unknown method");
            return ResponseFrame::err(
                &request_id,
                ErrorShape::new(
                    error_codes::UNKNOWN_METHOD,
                    format!("unknown method: {method}"),
                ),
            );
        };

        debug!(method, request_id = %request_id, conn_id = %conn_id, "dispatching method");
        match handler(ctx).await {
            Ok(payload) => {
                debug!(method, request_id = %request_id, "method ok");
                ResponseFrame::ok(&request_id, payload)
            },
            Err(err) => {
                if err.code == error_codes::UNAVAILABLE {
                    debug!(method, request_id = %request_id, code = %err.code, msg = %err.message, "method unavailable");
                } else {
                    warn!(method, request_id = %request_id, code = %err.code, msg = %err.message, "method error");
                }
                ResponseFrame::err(&request_id, err)
            },
        }
    }

    pub fn method_names(&self) -> Vec<String> {
        let mut names: Vec<_> = self.handlers.keys().cloned().collect();
        names.sort();
        names
    }

    fn register_defaults(&mut self) {
        super::a2ui::register(self);
        super::gateway::register(self);
        super::location::register(self);
        super::services::register(self);
        super::subscribe::register(self);
        super::channel_mux::register(self);
    }
}

/// Load the disabled hooks set from `data_dir/disabled_hooks.json`.
pub(crate) fn load_disabled_hooks() -> std::collections::HashSet<String> {
    let path = chelix_config::data_dir().join("disabled_hooks.json");
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn scopes(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    fn assert_error_code(result: Option<ErrorShape>, expected_code: &str) {
        let err = result.expect("expected an error");
        assert_eq!(err.code, expected_code, "wrong error code: {}", err.message);
    }

    #[test]
    fn senders_list_requires_read() {
        assert!(
            authorize_method(
                "channels.senders.list",
                "operator",
                &scopes(&["operator.read"])
            )
            .is_none()
        );
        assert_error_code(
            authorize_method("channels.senders.list", "operator", &scopes(&[])),
            "UNAUTHORIZED",
        );
    }

    #[test]
    fn senders_approve_requires_write() {
        assert!(
            authorize_method(
                "channels.senders.approve",
                "operator",
                &scopes(&["operator.write"])
            )
            .is_none()
        );
        assert_error_code(
            authorize_method(
                "channels.senders.approve",
                "operator",
                &scopes(&["operator.read"]),
            ),
            "UNAUTHORIZED",
        );
    }

    #[test]
    fn senders_deny_requires_write() {
        assert!(
            authorize_method(
                "channels.senders.deny",
                "operator",
                &scopes(&["operator.write"])
            )
            .is_none()
        );
        assert_error_code(
            authorize_method(
                "channels.senders.deny",
                "operator",
                &scopes(&["operator.read"]),
            ),
            "UNAUTHORIZED",
        );
    }

    #[test]
    fn admin_scope_allows_all_sender_methods() {
        for method in &[
            "channels.senders.list",
            "channels.senders.approve",
            "channels.senders.deny",
        ] {
            assert!(
                authorize_method(method, "operator", &scopes(&["operator.admin"])).is_none(),
                "admin should authorize {method}"
            );
        }
    }

    #[test]
    fn node_role_denied_sender_methods() {
        for method in &[
            "channels.senders.list",
            "channels.senders.approve",
            "channels.senders.deny",
        ] {
            assert_error_code(
                authorize_method(method, "node", &scopes(&["operator.admin"])),
                "FORBIDDEN",
            );
        }
    }

    #[test]
    fn user_get_requires_read() {
        assert!(authorize_method("user.get", "operator", &scopes(&["operator.read"])).is_none());
        assert_error_code(
            authorize_method("user.get", "operator", &scopes(&[])),
            "UNAUTHORIZED",
        );
    }

    #[test]
    fn user_update_requires_write() {
        assert!(
            authorize_method("user.update", "operator", &scopes(&["operator.write"])).is_none()
        );
        assert_error_code(
            authorize_method("user.update", "operator", &scopes(&["operator.read"])),
            "UNAUTHORIZED",
        );
    }

    #[test]
    fn cron_read_methods_require_read() {
        for method in &["cron.list", "cron.status", "cron.runs"] {
            assert!(
                authorize_method(method, "operator", &scopes(&["operator.read"])).is_none(),
                "read scope should authorize {method}"
            );
            assert_error_code(
                authorize_method(method, "operator", &scopes(&[])),
                "UNAUTHORIZED",
            );
        }
    }

    #[test]
    fn cron_write_methods_require_write() {
        for method in &["cron.add", "cron.update", "cron.remove", "cron.run"] {
            assert!(
                authorize_method(method, "operator", &scopes(&["operator.write"])).is_none(),
                "write scope should authorize {method}"
            );
            assert_error_code(
                authorize_method(method, "operator", &scopes(&["operator.read"])),
                "UNAUTHORIZED",
            );
        }
    }

    #[test]
    fn hooks_list_requires_read() {
        assert!(authorize_method("hooks.list", "operator", &scopes(&["operator.read"])).is_none());
        assert_error_code(
            authorize_method("hooks.list", "operator", &scopes(&[])),
            "UNAUTHORIZED",
        );
    }

    #[test]
    fn hooks_write_methods_require_write() {
        for method in &[
            "hooks.enable",
            "hooks.disable",
            "hooks.save",
            "hooks.reload",
        ] {
            assert!(
                authorize_method(method, "operator", &scopes(&["operator.write"])).is_none(),
                "write scope should authorize {method}"
            );
            assert_error_code(
                authorize_method(method, "operator", &scopes(&["operator.read"])),
                "UNAUTHORIZED",
            );
        }
    }

    #[test]
    fn unknown_method_returns_unknown_code() {
        use crate::{
            auth::{AuthMode, ResolvedAuth},
            services::GatewayServices,
            state::GatewayState,
        };

        let reg = MethodRegistry::new();
        let ctx = MethodContext {
            request_id: "test".into(),
            method: "nonexistent.method".into(),
            params: serde_json::Value::Null,
            client_conn_id: "conn-1".into(),
            transport: MethodTransport::StatefulConnection,
            client_role: "operator".into(),
            client_scopes: scopes(&["operator.admin"]),
            state: GatewayState::new(
                ResolvedAuth {
                    mode: AuthMode::Token,
                    token: None,
                    password: None,
                },
                GatewayServices::noop(),
            ),
            channel: None,
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        let resp = rt.block_on(reg.dispatch(ctx));
        assert!(!resp.ok);
        assert_eq!(
            resp.error.as_ref().map(|e| e.code.as_str()),
            Some("UNKNOWN_METHOD")
        );
    }

    #[test]
    fn chat_send_sync_is_registered_and_authorized() {
        let reg = MethodRegistry::new();
        assert!(reg.method_names().contains(&"chat.send_sync".to_string()));
        assert!(
            authorize_method("chat.send_sync", "operator", &scopes(&["operator.write"])).is_none()
        );
    }

    #[test]
    fn mcp_config_update_invalid_type_returns_invalid_message() {
        use crate::{
            auth::{AuthMode, ResolvedAuth},
            services::GatewayServices,
            state::GatewayState,
        };

        let reg = MethodRegistry::new();
        let ctx = MethodContext {
            request_id: "test".into(),
            method: "mcp.config.update".into(),
            params: serde_json::json!({
                "request_timeout_secs": "oops"
            }),
            client_conn_id: "conn-1".into(),
            transport: MethodTransport::StatefulConnection,
            client_role: "operator".into(),
            client_scopes: scopes(&["operator.write"]),
            state: GatewayState::new(
                ResolvedAuth {
                    mode: AuthMode::Token,
                    token: None,
                    password: None,
                },
                GatewayServices::noop(),
            ),
            channel: None,
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        let resp = rt.block_on(reg.dispatch(ctx));
        assert!(!resp.ok);
        assert_eq!(
            resp.error.as_ref().map(|e| e.message.as_str()),
            Some("invalid 'request_timeout_secs' parameter: expected a positive integer")
        );
    }

    #[test]
    fn session_resolution_stays_within_transport_mode() {
        use crate::{
            auth::{AuthMode, ResolvedAuth},
            services::GatewayServices,
            state::GatewayState,
        };

        let state = GatewayState::new(
            ResolvedAuth {
                mode: AuthMode::Token,
                token: None,
                password: None,
            },
            GatewayServices::noop(),
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            state
                .client_registry
                .write()
                .await
                .active_sessions
                .insert("stateful".into(), "session:stateful".into());
        });

        let cases = [
            (
                "stateful exact",
                "stateful",
                MethodTransport::StatefulConnection,
                Some("session:stateful"),
            ),
            (
                "stateful missing",
                "missing",
                MethodTransport::StatefulConnection,
                None,
            ),
            (
                "stateless exact",
                "stateful",
                MethodTransport::StatelessHttp {
                    session_id: Some(SessionKey::new("session:http")),
                },
                Some("session:http"),
            ),
            (
                "stateless missing does not use connection state",
                "stateful",
                MethodTransport::StatelessHttp { session_id: None },
                None,
            ),
        ];

        for (name, client_conn_id, transport, expected) in cases {
            let context = MethodContext {
                request_id: name.into(),
                method: "chat.send".into(),
                params: serde_json::Value::Null,
                client_conn_id: client_conn_id.into(),
                transport,
                client_role: "operator".into(),
                client_scopes: scopes(&["operator.write"]),
                state: Arc::clone(&state),
                channel: None,
            };
            let actual = runtime.block_on(context.resolved_session_id());
            assert_eq!(actual.as_ref().map(SessionKey::as_str), expected, "{name}");
        }
    }

    #[test]
    fn subscribe_method_authorized_with_write() {
        assert!(authorize_method("subscribe", "operator", &scopes(&["operator.write"])).is_none());
    }

    #[test]
    fn channel_join_authorized_with_write() {
        assert!(
            authorize_method("channel.join", "operator", &scopes(&["operator.write"])).is_none()
        );
    }

    #[test]
    fn system_describe_authorized_with_read() {
        assert!(
            authorize_method("system.describe", "operator", &scopes(&["operator.read"])).is_none()
        );
    }
}
