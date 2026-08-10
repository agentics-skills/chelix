use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(feature = "agent")]
use std::path::Component;

use tracing::warn;

#[cfg(feature = "agent")]
use chelix_agents::prompt::WorkspaceFilePromptStatus;
use chelix_protocol::{ErrorShape, error_codes};

use crate::{
    broadcast::{BroadcastOpts, broadcast},
    services::ServiceError,
};

use super::{MethodContext, MethodRegistry};

async fn active_session_key_for_ctx(ctx: &MethodContext) -> Option<String> {
    ctx.state
        .client_registry
        .read()
        .await
        .active_sessions
        .get(&ctx.client_conn_id)
        .cloned()
}

async fn default_agent_id_for_ctx(ctx: &MethodContext) -> Result<String, ErrorShape> {
    let agents = ctx.state.services.agents_config.as_ref().ok_or_else(|| {
        ErrorShape::new(
            error_codes::UNAVAILABLE,
            "agent configuration is not available",
        )
    })?;
    let guard = agents.read().await;
    if guard.default.trim().is_empty() {
        return Err(ErrorShape::new(
            error_codes::INTERNAL,
            "agents.default is not configured",
        ));
    }
    if !guard.entries.contains_key(&guard.default) {
        return Err(ErrorShape::new(
            error_codes::INTERNAL,
            format!("default agent '{}' not found", guard.default),
        ));
    }
    Ok(guard.default.clone())
}

async fn agent_exists_for_ctx(ctx: &MethodContext, agent_id: &str) -> bool {
    let Some(agents) = ctx.state.services.agents_config.as_ref() else {
        return false;
    };
    agents.read().await.entries.contains_key(agent_id)
}

async fn ensure_agent_exists_for_ctx(
    ctx: &MethodContext,
    agent_id: &str,
) -> Result<(), ErrorShape> {
    if agent_exists_for_ctx(ctx, agent_id).await {
        return Ok(());
    }
    Err(ErrorShape::new(
        error_codes::INVALID_REQUEST,
        format!("agent '{agent_id}' not found"),
    ))
}

#[cfg(feature = "agent")]
fn parse_agent_id_param(params: &serde_json::Value) -> Option<String> {
    params
        .get("agent_id")
        .or_else(|| params.get("id"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

#[cfg(feature = "agent")]
async fn resolve_requested_agent_id(
    ctx: &MethodContext,
    params: &serde_json::Value,
) -> Result<String, ErrorShape> {
    if let Some(id) = parse_agent_id_param(params) {
        ensure_agent_exists_for_ctx(ctx, &id).await?;
        return Ok(id);
    }
    default_agent_id_for_ctx(ctx).await
}

fn agent_not_found(agent_id: &str) -> ErrorShape {
    ErrorShape::new(
        error_codes::INVALID_REQUEST,
        format!("agent '{agent_id}' not found"),
    )
}

#[cfg(feature = "agent")]
fn normalize_relative_agent_path(path: &str) -> Result<PathBuf, ErrorShape> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(ErrorShape::new(
            error_codes::INVALID_REQUEST,
            "missing 'path' parameter",
        ));
    }
    let candidate = Path::new(trimmed);
    if candidate.is_absolute() {
        return Err(ErrorShape::new(
            error_codes::INVALID_REQUEST,
            "path must be relative",
        ));
    }
    for component in candidate.components() {
        if matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            return Err(ErrorShape::new(
                error_codes::INVALID_REQUEST,
                "path traversal is not allowed",
            ));
        }
    }
    Ok(candidate.to_path_buf())
}

#[derive(Debug, Clone, serde::Serialize)]
#[cfg(feature = "agent")]
struct WorkspacePromptFileStatusResponse {
    path: String,
    source: &'static str,
    size: Option<u64>,
    #[serde(flatten)]
    prompt_status: WorkspaceFilePromptStatus,
}

#[cfg(feature = "agent")]
fn workspace_file_limit_chars(ctx: &MethodContext) -> usize {
    ctx.state.config.chat.workspace_file_max_chars
}

fn invalid_memory_config_value(field: &str, value: &str) -> ErrorShape {
    ErrorShape::new(
        error_codes::INVALID_REQUEST,
        format!("invalid memory config value for '{field}': '{value}'"),
    )
}

fn parse_memory_style(value: &str) -> Result<chelix_config::MemoryStyle, ErrorShape> {
    match value {
        "hybrid" => Ok(chelix_config::MemoryStyle::Hybrid),
        "prompt-only" => Ok(chelix_config::MemoryStyle::PromptOnly),
        "search-only" => Ok(chelix_config::MemoryStyle::SearchOnly),
        "off" => Ok(chelix_config::MemoryStyle::Off),
        _ => Err(invalid_memory_config_value("style", value)),
    }
}

fn parse_agent_memory_write_mode(
    value: &str,
) -> Result<chelix_config::AgentMemoryWriteMode, ErrorShape> {
    match value {
        "hybrid" => Ok(chelix_config::AgentMemoryWriteMode::Hybrid),
        "prompt-only" => Ok(chelix_config::AgentMemoryWriteMode::PromptOnly),
        "search-only" => Ok(chelix_config::AgentMemoryWriteMode::SearchOnly),
        "off" => Ok(chelix_config::AgentMemoryWriteMode::Off),
        _ => Err(invalid_memory_config_value("agent_write_mode", value)),
    }
}

fn parse_user_profile_write_mode(
    value: &str,
) -> Result<chelix_config::UserProfileWriteMode, ErrorShape> {
    match value {
        "explicit-and-auto" => Ok(chelix_config::UserProfileWriteMode::ExplicitAndAuto),
        "explicit-only" => Ok(chelix_config::UserProfileWriteMode::ExplicitOnly),
        "off" => Ok(chelix_config::UserProfileWriteMode::Off),
        _ => Err(invalid_memory_config_value(
            "user_profile_write_mode",
            value,
        )),
    }
}

fn parse_memory_backend(value: &str) -> Result<chelix_config::MemoryBackend, ErrorShape> {
    match value {
        "builtin" => Ok(chelix_config::MemoryBackend::Builtin),
        "qmd" => Ok(chelix_config::MemoryBackend::Qmd),
        _ => Err(invalid_memory_config_value("backend", value)),
    }
}

fn parse_memory_provider(value: &str) -> Result<Option<chelix_config::MemoryProvider>, ErrorShape> {
    match value {
        "auto" => Ok(None),
        "local" => Ok(Some(chelix_config::MemoryProvider::Local)),
        "openai" => Ok(Some(chelix_config::MemoryProvider::OpenAi)),
        "custom" => Ok(Some(chelix_config::MemoryProvider::Custom)),
        _ => Err(invalid_memory_config_value("provider", value)),
    }
}

fn parse_memory_citations_mode(
    value: &str,
) -> Result<chelix_config::MemoryCitationsMode, ErrorShape> {
    match value {
        "on" => Ok(chelix_config::MemoryCitationsMode::On),
        "off" => Ok(chelix_config::MemoryCitationsMode::Off),
        "auto" => Ok(chelix_config::MemoryCitationsMode::Auto),
        _ => Err(invalid_memory_config_value("citations", value)),
    }
}

fn parse_memory_search_merge_strategy(
    value: &str,
) -> Result<chelix_config::MemorySearchMergeStrategy, ErrorShape> {
    match value {
        "rrf" => Ok(chelix_config::MemorySearchMergeStrategy::Rrf),
        "linear" => Ok(chelix_config::MemorySearchMergeStrategy::Linear),
        _ => Err(invalid_memory_config_value("search_merge_strategy", value)),
    }
}

fn parse_session_export_mode(
    value: &serde_json::Value,
) -> Result<chelix_config::SessionExportMode, ErrorShape> {
    match value {
        serde_json::Value::Bool(false) => Ok(chelix_config::SessionExportMode::Off),
        serde_json::Value::Bool(true) => Ok(chelix_config::SessionExportMode::OnNewOrReset),
        serde_json::Value::String(string) => match string.as_str() {
            "off" => Ok(chelix_config::SessionExportMode::Off),
            "on-new-or-reset" => Ok(chelix_config::SessionExportMode::OnNewOrReset),
            _ => Err(invalid_memory_config_value("session_export", string)),
        },
        _ => Err(ErrorShape::new(
            error_codes::INVALID_REQUEST,
            "invalid memory config value for 'session_export': expected bool or string",
        )),
    }
}

fn parse_prompt_memory_mode(value: &str) -> Result<chelix_config::PromptMemoryMode, ErrorShape> {
    match value {
        "live-reload" => Ok(chelix_config::PromptMemoryMode::LiveReload),
        "frozen-at-session-start" => Ok(chelix_config::PromptMemoryMode::FrozenAtSessionStart),
        _ => Err(invalid_memory_config_value("prompt_memory_mode", value)),
    }
}

#[cfg(feature = "agent")]
fn should_fallback_agent_file_to_root(relative_path: &Path) -> bool {
    matches!(relative_path.to_str(), Some("AGENTS.md") | Some("TOOLS.md"))
}

#[cfg(feature = "agent")]
fn resolve_agent_file_target(
    agent_id: &str,
    relative_path: &Path,
) -> Option<(PathBuf, &'static str)> {
    let primary = chelix_config::agent_workspace_dir(agent_id).join(relative_path);
    if primary.exists() {
        return Some((primary, "agent"));
    }

    if should_fallback_agent_file_to_root(relative_path) {
        let fallback = chelix_config::data_dir().join(relative_path);
        if fallback.exists() {
            return Some((fallback, "root"));
        }
    }

    None
}

#[cfg(feature = "agent")]
fn workspace_prompt_file_status(
    agent_id: &str,
    file_name: &str,
    limit_chars: usize,
) -> Option<WorkspacePromptFileStatusResponse> {
    let relative_path = Path::new(file_name);
    let (path, source) = resolve_agent_file_target(agent_id, relative_path)?;
    let content = std::fs::read_to_string(&path).ok()?;
    let normalized = chelix_config::normalize_workspace_markdown_content(&content)?;
    let original_chars = normalized.chars().count();
    let size_bytes = std::fs::metadata(&path).ok().map(|meta| meta.len());
    Some(WorkspacePromptFileStatusResponse {
        path: file_name.to_string(),
        source,
        size: size_bytes,
        prompt_status: WorkspaceFilePromptStatus {
            name: file_name.to_string(),
            original_chars,
            included_chars: original_chars.min(limit_chars),
            limit_chars,
            truncated_chars: original_chars.saturating_sub(limit_chars),
            truncated: original_chars > limit_chars,
        },
    })
}

#[cfg(feature = "agent")]
fn read_agent_file(agent_id: &str, relative_path: &Path) -> Result<String, ErrorShape> {
    let (target, _) = resolve_agent_file_target(agent_id, relative_path)
        .ok_or_else(|| ErrorShape::new(error_codes::INVALID_REQUEST, "file not found"))?;

    std::fs::read_to_string(target)
        .map_err(|e| ErrorShape::new(error_codes::UNAVAILABLE, e.to_string()))
}

#[cfg(feature = "agent")]
fn list_agent_workspace_files_recursively(
    root: &Path,
    base: &Path,
    files: &mut Vec<serde_json::Value>,
) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            list_agent_workspace_files_recursively(&path, base, files);
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        if let Ok(relative) = path.strip_prefix(base) {
            files.push(serde_json::json!({
                "path": relative.to_string_lossy(),
                "size": entry.metadata().ok().map(|m| m.len()),
            }));
        }
    }
}

mod admin;
mod agents;
mod channels;
mod core;
mod sessions;
mod system;
mod voice_personas;
mod voicecall;

pub(super) fn register(reg: &mut MethodRegistry) {
    agents::register(reg);
    sessions::register(reg);
    channels::register(reg);
    core::register(reg);
    system::register(reg);
    admin::register(reg);
    voice_personas::register(reg);
    voicecall::register(reg);
}
async fn reload_hooks(state: &Arc<crate::state::GatewayState>) -> Result<(), ErrorShape> {
    let disabled = state.inner.read().await.disabled_hooks.clone();
    let session_store = state.services.session_store.as_ref();
    let (new_registry, new_info) =
        crate::server::discover_and_build_hooks(&disabled, session_store)
            .await
            .map_err(|error| ErrorShape::new(error_codes::INTERNAL, error.to_string()))?;

    {
        let mut inner = state.inner.write().await;
        inner.hook_registry = new_registry;
        inner.discovered_hooks = new_info.clone();
    }

    // Broadcast hooks.status event so connected UIs auto-refresh.
    broadcast(
        state,
        "hooks.status",
        serde_json::json!({ "hooks": new_info }),
        BroadcastOpts::default(),
    )
    .await;
    Ok(())
}

/// Persist the disabled hooks set to `data_dir/disabled_hooks.json`.
async fn persist_disabled_hooks(state: &Arc<crate::state::GatewayState>) {
    let disabled = state.inner.read().await.disabled_hooks.clone();
    let path = chelix_config::data_dir().join("disabled_hooks.json");
    let json = serde_json::to_string_pretty(&disabled).unwrap_or_default();
    if let Err(e) = std::fs::write(&path, json) {
        warn!("failed to persist disabled hooks: {e}");
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{
            auth::{AuthMode, ResolvedAuth},
            services::GatewayServices,
            state::GatewayState,
        },
        tempfile::TempDir,
    };

    #[cfg(feature = "agent")]
    #[test]
    fn agent_root_fallback_is_independent_of_agent_id() {
        assert!(should_fallback_agent_file_to_root(Path::new("AGENTS.md")));
        assert!(should_fallback_agent_file_to_root(Path::new("TOOLS.md")));
        assert!(!should_fallback_agent_file_to_root(Path::new("SOUL.md")));
        assert!(!should_fallback_agent_file_to_root(Path::new(
            "notes/plan.md"
        )));
    }

    struct MemoryConfigTestGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        _config_dir: TempDir,
        _data_dir: TempDir,
    }

    impl MemoryConfigTestGuard {
        fn new() -> Self {
            let lock = crate::config_override_test_lock();
            let config_dir = tempfile::tempdir()
                .unwrap_or_else(|error| panic!("config tempdir should be created: {error}"));
            let data_dir = tempfile::tempdir()
                .unwrap_or_else(|error| panic!("data tempdir should be created: {error}"));
            chelix_config::set_config_dir(config_dir.path().to_path_buf());
            chelix_config::set_data_dir(data_dir.path().to_path_buf());
            chelix_config::initialize_config()
                .unwrap_or_else(|error| panic!("test config should be initialized: {error}"));
            Self {
                _lock: lock,
                _config_dir: config_dir,
                _data_dir: data_dir,
            }
        }
    }

    impl Drop for MemoryConfigTestGuard {
        fn drop(&mut self) {
            chelix_config::clear_config_dir();
            chelix_config::clear_data_dir();
        }
    }

    async fn dispatch_memory_method(method: &str, params: serde_json::Value) -> serde_json::Value {
        let mut reg = MethodRegistry::default();
        register(&mut reg);
        let response = reg
            .dispatch(MethodContext {
                request_id: "test".into(),
                method: method.to_string(),
                params,
                client_conn_id: "conn-1".into(),
                client_role: "operator".into(),
                client_scopes: vec!["operator.write".into(), "operator.read".into()],
                state: GatewayState::new(
                    ResolvedAuth {
                        mode: AuthMode::Token,
                        token: None,
                        password: None,
                    },
                    GatewayServices::noop(),
                ),
                channel: None,
            })
            .await;

        assert!(response.ok, "method failed: {:?}", response.error);
        match response.payload {
            Some(payload) => payload,
            None => panic!("method {method} returned no payload"),
        }
    }

    async fn dispatch_memory_method_response(
        method: &str,
        params: serde_json::Value,
    ) -> chelix_protocol::ResponseFrame {
        let mut reg = MethodRegistry::default();
        register(&mut reg);
        reg.dispatch(MethodContext {
            request_id: "test".into(),
            method: method.to_string(),
            params,
            client_conn_id: "conn-1".into(),
            client_role: "operator".into(),
            client_scopes: vec!["operator.write".into(), "operator.read".into()],
            state: GatewayState::new(
                ResolvedAuth {
                    mode: AuthMode::Token,
                    token: None,
                    password: None,
                },
                GatewayServices::noop(),
            ),
            channel: None,
        })
        .await
    }

    #[tokio::test]
    async fn memory_config_get_reports_typed_memory_fields() {
        let _guard = MemoryConfigTestGuard::new();
        let update_result = chelix_config::update_config(|cfg| {
            cfg.memory.style = chelix_config::MemoryStyle::SearchOnly;
            cfg.memory.agent_write_mode = chelix_config::AgentMemoryWriteMode::PromptOnly;
            cfg.memory.user_profile_write_mode = chelix_config::UserProfileWriteMode::ExplicitOnly;
            cfg.memory.backend = chelix_config::MemoryBackend::Qmd;
            cfg.memory.provider = Some(chelix_config::MemoryProvider::OpenAi);
            cfg.memory.citations = chelix_config::MemoryCitationsMode::Off;
            cfg.memory.disable_rag = true;
            cfg.memory.llm_reranking = true;
            cfg.memory.search_merge_strategy = chelix_config::MemorySearchMergeStrategy::Linear;
            cfg.memory.session_export = chelix_config::SessionExportMode::Off;
            cfg.chat.prompt_memory_mode = chelix_config::PromptMemoryMode::FrozenAtSessionStart;
        });
        assert!(update_result.is_ok(), "config update should succeed");

        let payload = dispatch_memory_method("memory.config.get", serde_json::json!({})).await;
        assert_eq!(payload["style"], "search-only");
        assert_eq!(payload["agent_write_mode"], "prompt-only");
        assert_eq!(payload["user_profile_write_mode"], "explicit-only");
        assert_eq!(payload["backend"], "qmd");
        assert_eq!(payload["provider"], "openai");
        assert_eq!(payload["citations"], "off");
        assert_eq!(payload["disable_rag"], true);
        assert_eq!(payload["llm_reranking"], true);
        assert_eq!(payload["search_merge_strategy"], "linear");
        assert_eq!(payload["session_export"], "off");
        assert_eq!(payload["prompt_memory_mode"], "frozen-at-session-start");
    }

    #[tokio::test]
    async fn memory_config_update_persists_typed_memory_fields() {
        let _guard = MemoryConfigTestGuard::new();

        let payload = dispatch_memory_method(
            "memory.config.update",
            serde_json::json!({
                "style": "prompt-only",
                "agent_write_mode": "search-only",
                "user_profile_write_mode": "off",
                "backend": "qmd",
                "provider": "custom",
                "citations": "on",
                "disable_rag": true,
                "llm_reranking": true,
                "search_merge_strategy": "linear",
                "session_export": false,
                "prompt_memory_mode": "frozen-at-session-start",
            }),
        )
        .await;

        assert_eq!(payload["style"], "prompt-only");
        assert_eq!(payload["agent_write_mode"], "search-only");
        assert_eq!(payload["user_profile_write_mode"], "off");
        assert_eq!(payload["backend"], "qmd");
        assert_eq!(payload["provider"], "custom");
        assert_eq!(payload["citations"], "on");
        assert_eq!(payload["disable_rag"], true);
        assert_eq!(payload["llm_reranking"], true);
        assert_eq!(payload["search_merge_strategy"], "linear");
        assert_eq!(payload["session_export"], "off");
        assert_eq!(payload["prompt_memory_mode"], "frozen-at-session-start");

        let config = chelix_config::discover_and_load()
            .unwrap_or_else(|error| panic!("load updated config: {error}"));
        assert_eq!(config.memory.style, chelix_config::MemoryStyle::PromptOnly);
        assert_eq!(
            config.memory.agent_write_mode,
            chelix_config::AgentMemoryWriteMode::SearchOnly
        );
        assert_eq!(
            config.memory.user_profile_write_mode,
            chelix_config::UserProfileWriteMode::Off
        );
        assert_eq!(config.memory.backend, chelix_config::MemoryBackend::Qmd);
        assert_eq!(
            config.memory.provider,
            Some(chelix_config::MemoryProvider::Custom)
        );
        assert_eq!(
            config.memory.citations,
            chelix_config::MemoryCitationsMode::On
        );
        assert!(config.memory.disable_rag);
        assert!(config.memory.llm_reranking);
        assert_eq!(
            config.memory.search_merge_strategy,
            chelix_config::MemorySearchMergeStrategy::Linear
        );
        assert_eq!(
            config.memory.session_export,
            chelix_config::SessionExportMode::Off
        );
        assert_eq!(
            config.chat.prompt_memory_mode,
            chelix_config::PromptMemoryMode::FrozenAtSessionStart
        );
    }

    #[tokio::test]
    async fn memory_config_update_rejects_unknown_enum_values() {
        let _guard = MemoryConfigTestGuard::new();
        let response = dispatch_memory_method_response(
            "memory.config.update",
            serde_json::json!({
                "style": "surprise-mode",
            }),
        )
        .await;

        assert!(!response.ok, "invalid enum value should fail");
        let error = match response.error {
            Some(error) => error,
            None => panic!("expected invalid request error"),
        };
        assert_eq!(error.code, error_codes::INVALID_REQUEST);
        assert_eq!(
            error.message,
            "invalid memory config value for 'style': 'surprise-mode'"
        );
    }
}
