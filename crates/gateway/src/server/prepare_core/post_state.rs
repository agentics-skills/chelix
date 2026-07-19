use std::{
    path::PathBuf,
    sync::{Arc, atomic::Ordering},
};

use {
    secrecy::Secret,
    tracing::{info, warn},
};

#[cfg(feature = "wasm")]
use secrecy::ExposeSecret;

mod credential_env;
mod session_tools;

use credential_env::{CredentialEnvVarProvider, ensure_sandbox_api_key};

use {
    chelix_providers::ProviderRegistry,
    chelix_sessions::{
        metadata::SqliteSessionMetadata, session_events::SessionEventBus, store::SessionStore,
    },
};

use crate::{
    approval::GatewayApprovalBroadcaster,
    auth,
    broadcast::{BroadcastOpts, broadcast},
    chat::{LiveChatService, LiveModelService},
    external_agents::{
        ExternalAgentChatService, ExternalAgentSessionService, GatewayExternalAgentService,
    },
    methods::MethodRegistry,
    provider_setup::LiveProviderSetupService,
    services::GatewayServices,
    state::{DiscoveredHookInfo, GatewayState},
};

use crate::server::{
    helpers::{StartupMemProbe, env_flag_enabled, instance_slug},
    prepared::PreparedGatewayCore,
};

#[cfg(feature = "wasm")]
use crate::server::helpers::env_value_with_overrides;

#[cfg(feature = "file-watcher")]
use crate::server::helpers::start_skill_hot_reload_watcher;

pub(super) struct PostStateInputs {
    pub bind: String,
    pub port: u16,
    pub config: chelix_config::ChelixConfig,
    pub log_buffer: Option<crate::logs::LogBuffer>,
    pub data_dir: PathBuf,
    pub resolved_auth: auth::ResolvedAuth,
    pub deploy_platform: Option<String>,
    pub session_event_bus: SessionEventBus,
    pub services: GatewayServices,
    pub session_mutations: Arc<chelix_service_traits::SessionMutationCoordinator>,
    pub registry: Arc<tokio::sync::RwLock<ProviderRegistry>>,
    pub runtime_env_overrides: std::collections::HashMap<String, String>,
    pub provider_summary: String,
    pub mcp_configured_count: usize,
    pub model_store: Arc<tokio::sync::RwLock<crate::chat::DisabledModelsStore>>,
    pub live_model_service: Arc<LiveModelService>,
    pub provider_setup_service: Arc<LiveProviderSetupService>,
    pub live_mcp: Arc<crate::mcp_service::LiveMcpService>,
    pub memory_manager: Option<chelix_memory::runtime::DynMemoryRuntime>,
    pub code_index: Arc<chelix_code_index::CodeIndex>,
    #[cfg(any(feature = "qmd", feature = "code-index-builtin"))]
    pub project_store: Arc<dyn chelix_projects::ProjectStore>,
    pub credential_store: Arc<auth::CredentialStore>,
    pub db_pool: sqlx::SqlitePool,
    pub session_store: Arc<SessionStore>,
    pub session_metadata: Arc<SqliteSessionMetadata>,
    pub session_share_store: Arc<crate::share_store::ShareStore>,
    pub session_state_store: Arc<chelix_sessions::state_store::SessionStateStore>,
    pub agent_persona_store: Arc<crate::agent_persona::AgentPersonaStore>,
    pub sandbox_router: Arc<chelix_tools::sandbox::SandboxRouter>,
    pub tools_service: Arc<dyn chelix_tools::tools_service::ToolsService>,
    pub cron_service: Arc<chelix_cron::service::CronService>,
    pub deferred_state: Arc<tokio::sync::OnceCell<Arc<GatewayState>>>,
    pub startup_mem_probe: StartupMemProbe,
    pub approval_manager: Arc<chelix_tools::approval::ApprovalManager>,
    pub hook_registry: Option<Arc<chelix_common::hooks::HookRegistry>>,
    pub discovered_hooks_info: Vec<DiscoveredHookInfo>,
    pub persisted_disabled: std::collections::HashSet<String>,
    pub agents_config: Arc<tokio::sync::RwLock<chelix_config::AgentsConfig>>,
    #[cfg(feature = "msteams")]
    pub msteams_webhook_plugin: Arc<tokio::sync::RwLock<chelix_msteams::MsTeamsPlugin>>,
    #[cfg(feature = "slack")]
    pub slack_webhook_plugin: Arc<tokio::sync::RwLock<chelix_slack::SlackPlugin>>,
    #[cfg(feature = "telephony")]
    pub telephony_webhook_plugin: Arc<tokio::sync::RwLock<chelix_telephony::TelephonyPlugin>>,
    #[cfg(feature = "vault")]
    pub vault: Option<Arc<chelix_vault::Vault>>,
}

async fn build_webauthn_registry(
    config: &chelix_config::ChelixConfig,
    port: u16,
) -> anyhow::Result<Option<crate::auth_webauthn::SharedWebAuthnRegistry>> {
    let default_scheme = if config.tls.enabled {
        "https"
    } else {
        "http"
    };

    // Derive RP ID and origin from server.external_url / CHELIX_EXTERNAL_URL
    // when available, before falling back to fine-grained env vars.
    let (external_rp_id, external_origin) = if let Some(ref ext_url) =
        config.server.effective_external_url()
    {
        match url::Url::parse(ext_url) {
            Ok(parsed) => {
                let host = parsed.host_str().unwrap_or_default().to_string();
                if host.is_empty() {
                    warn!(
                        "server.external_url '{ext_url}' parsed successfully but has no hostname; ignoring"
                    );
                    (None, None)
                } else {
                    (Some(host), Some(ext_url.clone()))
                }
            },
            Err(e) => {
                warn!("invalid server.external_url '{ext_url}': {e}");
                (None, None)
            },
        }
    } else {
        (None, None)
    };

    let explicit_rp_id = external_rp_id
        .or_else(|| std::env::var("CHELIX_WEBAUTHN_RP_ID").ok())
        .or_else(|| std::env::var("APP_DOMAIN").ok());
    let explicit_origin = external_origin
        .or_else(|| std::env::var("CHELIX_WEBAUTHN_ORIGIN").ok())
        .or_else(|| std::env::var("APP_URL").ok());

    let mut wa_registry = crate::auth_webauthn::WebAuthnRegistry::new();
    let mut any_ok = false;

    let mut try_add = |rp_id: &str, origin_str: &str, extras: &[webauthn_rs::prelude::Url]| {
        let rp_id = crate::auth_webauthn::normalize_host(rp_id);
        if rp_id.is_empty() || wa_registry.contains_host(&rp_id) {
            return;
        }
        let Ok(origin_url) = webauthn_rs::prelude::Url::parse(origin_str) else {
            tracing::warn!("invalid WebAuthn origin URL '{origin_str}'");
            return;
        };
        match crate::auth_webauthn::WebAuthnState::new(&rp_id, &origin_url, extras) {
            Ok(wa) => {
                info!(rp_id = %rp_id, origins = ?wa.get_allowed_origins(), "WebAuthn RP registered");
                wa_registry.add(rp_id.clone(), wa);
                any_ok = true;
            },
            Err(e) => tracing::warn!(rp_id = %rp_id, "failed to init WebAuthn: {e}"),
        }
    };

    if let Some(ref rp_id) = explicit_rp_id {
        let origin = explicit_origin
            .clone()
            .unwrap_or_else(|| format!("https://{rp_id}"));
        try_add(rp_id, &origin, &[]);
    } else {
        let localhost_origin = format!("{default_scheme}://localhost:{port}");
        let chelix_localhost: Vec<webauthn_rs::prelude::Url> = webauthn_rs::prelude::Url::parse(
            &format!("{default_scheme}://chelix.localhost:{port}"),
        )
        .into_iter()
        .collect();
        try_add("localhost", &localhost_origin, &chelix_localhost);

        let instance_slug_value = instance_slug(config);
        if instance_slug_value != "localhost" {
            let bot_origin = format!("{default_scheme}://{instance_slug_value}:{port}");
            try_add(&instance_slug_value, &bot_origin, &[]);

            let bot_local = format!("{instance_slug_value}.local");
            let bot_local_origin = format!("{default_scheme}://{bot_local}:{port}");
            try_add(&bot_local, &bot_local_origin, &[]);
        }

        if let Ok(hn) = hostname::get() {
            let hn_str = hn.to_string_lossy();
            if hn_str != "localhost" {
                let local_name = if hn_str.ends_with(".local") {
                    hn_str.to_string()
                } else {
                    format!("{hn_str}.local")
                };
                let local_origin = format!("{default_scheme}://{local_name}:{port}");
                try_add(&local_name, &local_origin, &[]);

                let bare = hn_str.strip_suffix(".local").unwrap_or(&hn_str);
                if bare != local_name {
                    let bare_origin = format!("{default_scheme}://{bare}:{port}");
                    try_add(bare, &bare_origin, &[]);
                }
            }
        }
    }

    if any_ok {
        info!(origins = ?wa_registry.get_all_origins(), "WebAuthn passkeys enabled");
        Ok(Some(Arc::new(tokio::sync::RwLock::new(wa_registry))))
    } else {
        Ok(None)
    }
}

#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
pub(super) async fn complete_startup(
    inputs: PostStateInputs,
) -> anyhow::Result<PreparedGatewayCore> {
    let PostStateInputs {
        bind,
        port,
        config,
        log_buffer,
        data_dir,
        resolved_auth,
        deploy_platform,
        session_event_bus,
        mut services,
        session_mutations,
        registry,
        runtime_env_overrides,
        provider_summary,
        mcp_configured_count,
        model_store,
        live_model_service,
        provider_setup_service,
        live_mcp,
        memory_manager,
        credential_store,
        db_pool,
        session_store,
        session_metadata,
        session_share_store: _session_share_store,
        session_state_store,
        agent_persona_store: _agent_persona_store,
        sandbox_router,
        tools_service,
        cron_service,
        deferred_state,
        mut startup_mem_probe,
        approval_manager,
        hook_registry,
        discovered_hooks_info,
        persisted_disabled,
        agents_config,
        #[cfg(feature = "msteams")]
        msteams_webhook_plugin,
        #[cfg(feature = "slack")]
        slack_webhook_plugin,
        #[cfg(feature = "telephony")]
        telephony_webhook_plugin,
        #[cfg(feature = "vault")]
        vault,
        code_index,
        #[cfg(any(feature = "qmd", feature = "code-index-builtin"))]
        project_store,
    } = inputs;

    let is_localhost =
        matches!(bind.as_str(), "127.0.0.1" | "::1" | "localhost") || bind.ends_with(".localhost");

    #[cfg(feature = "metrics")]
    let metrics_handle = {
        let metrics_config = chelix_metrics::MetricsRecorderConfig {
            enabled: config.metrics.enabled,
            prefix: None,
            global_labels: vec![
                ("service".to_string(), "chelix-gateway".to_string()),
                ("version".to_string(), chelix_config::VERSION.to_string()),
            ],
        };
        match chelix_metrics::init_metrics(metrics_config) {
            Ok(handle) => {
                if config.metrics.enabled {
                    info!("Metrics collection enabled");
                }
                Some(handle)
            },
            Err(e) => {
                warn!("Failed to initialize metrics: {e}");
                None
            },
        }
    };

    #[cfg(feature = "metrics")]
    let metrics_store: Option<Arc<dyn crate::state::MetricsStore>> = {
        let metrics_db_path = data_dir.join("metrics.db");
        match chelix_metrics::SqliteMetricsStore::new(&metrics_db_path).await {
            Ok(store) => {
                info!(
                    "Metrics history store initialized at {}",
                    metrics_db_path.display()
                );
                Some(Arc::new(store))
            },
            Err(e) => {
                warn!("Failed to initialize metrics store: {e}");
                None
            },
        }
    };

    let browser_for_lifecycle = Arc::clone(&services.browser);
    let pairing_store = Arc::new(crate::pairing::PairingStore::new(db_pool.clone()));
    #[cfg(feature = "tls")]
    let tls_enabled_for_gateway = config.tls.enabled;
    #[cfg(not(feature = "tls"))]
    let tls_enabled_for_gateway = false;

    #[cfg(feature = "qmd")]
    let code_index_for_tools = Arc::clone(&code_index);

    #[cfg(feature = "code-index-builtin")]
    #[allow(unused_variables)]
    let code_index_for_tools_builtin = Arc::clone(&code_index);

    #[cfg(feature = "telephony")]
    {
        services.telephony_plugin = Some(Arc::clone(&telephony_webhook_plugin));
    }

    let external_agent_service = Arc::new(GatewayExternalAgentService::new(
        config.external_agents.clone(),
        Arc::clone(&session_metadata),
        Arc::clone(&approval_manager),
    ));
    let session_service = Arc::clone(&services.session);
    services = services.with_session(Arc::new(ExternalAgentSessionService::new(
        session_service,
        Arc::clone(&external_agent_service),
    )));
    services = services.with_external_agent(external_agent_service.clone());

    let state = GatewayState::with_options(
        resolved_auth,
        services,
        config.clone(),
        Arc::clone(&sandbox_router),
        Some(Arc::clone(&credential_store)),
        Some(pairing_store),
        is_localhost,
        env_flag_enabled("CHELIX_BEHIND_PROXY"),
        tls_enabled_for_gateway,
        hook_registry.clone(),
        memory_manager.clone(),
        code_index,
        port,
        config.server.ws_request_logs,
        deploy_platform.clone(),
        Some(session_event_bus),
        #[cfg(feature = "metrics")]
        metrics_handle,
        #[cfg(feature = "metrics")]
        metrics_store.clone(),
        #[cfg(feature = "vault")]
        vault.clone(),
    );

    // Wire the shared LLM provider registry for lightweight generation
    // (auto-title, session summary, tts.generate_phrase).
    state.inner.write().await.llm_providers = Some(Arc::clone(&registry));

    {
        let (webhook_tx, webhook_rx) = tokio::sync::mpsc::channel::<i64>(256);
        let webhook_store: Arc<dyn chelix_webhooks::store::WebhookStore> = {
            let inner: Arc<dyn chelix_webhooks::store::WebhookStore> = Arc::new(
                chelix_webhooks::store::SqliteWebhookStore::with_pool(db_pool.clone()),
            );
            #[cfg(feature = "vault")]
            {
                Arc::new(crate::webhooks::VaultWebhookStore::new(
                    Arc::clone(&inner),
                    vault.clone(),
                ))
            }
            #[cfg(not(feature = "vault"))]
            {
                inner
            }
        };
        let _ = state.webhook_store.set(Arc::clone(&webhook_store));
        let _ = state.webhook_worker_tx.set(webhook_tx);

        let worker_store = Arc::clone(&webhook_store);
        let worker_state_ref = Arc::clone(&state);
        let worker = chelix_webhooks::worker::WebhookWorker::new(
            webhook_rx,
            worker_store,
            Arc::new(move |req: chelix_webhooks::worker::ExecuteRequest| {
                let chat_state = Arc::clone(&worker_state_ref);
                Box::pin(async move {
                    let chat = chat_state.chat();
                    let mut params = serde_json::json!({
                        "text": req.message,
                        "_session_key": req.session_key,
                    });
                    if let Some(ref model) = req.model {
                        params["model"] = serde_json::Value::String(model.clone());
                    }
                    if let Some(ref agent_id) = req.agent_id {
                        params["agent_id"] = serde_json::Value::String(agent_id.clone());
                    }
                    if let Some(ref tool_policy) = req.tool_policy {
                        params["_tool_policy"] = serde_json::to_value(tool_policy)
                            .map_err(|error| anyhow::anyhow!(error))?;
                    }
                    let result = chat
                        .send_sync(params)
                        .await
                        .map_err(|e| anyhow::anyhow!("{e}"))?;
                    let input_tokens = result.get("inputTokens").and_then(|v| v.as_i64());
                    let output_tokens = result.get("outputTokens").and_then(|v| v.as_i64());
                    let output = result
                        .get("text")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                    Ok(chelix_webhooks::worker::ProcessResult {
                        output,
                        input_tokens,
                        output_tokens,
                        session_key: req.session_key,
                    })
                })
            }),
        );
        tokio::spawn(worker.run());
    }

    let _ = deferred_state.set(Arc::clone(&state));

    provider_setup_service.set_broadcaster(Arc::new(crate::provider_setup::GatewayBroadcaster {
        state: Arc::clone(&state),
    }));
    live_model_service.set_state(crate::chat::GatewayChatRuntime::from_state(Arc::clone(
        &state,
    )));

    match credential_store.ssh_target_count().await {
        Ok(count) => state.ssh_target_count.store(count, Ordering::Relaxed),
        Err(error) => warn!(%error, "failed to load ssh target count"),
    }

    {
        let mut inner = state.inner.write().await;
        inner.discovered_hooks = discovered_hooks_info;
        inner.disabled_hooks = persisted_disabled;
        inner.shiki_cdn_url = config.server.shiki_cdn_url.clone();
        #[cfg(feature = "metrics")]
        {
            inner.metrics_history =
                crate::state::MetricsHistory::new(config.metrics.history_points);
        }
    }

    let setup_code_display =
        if !credential_store.is_setup_complete() && !credential_store.is_auth_disabled() {
            let code = std::env::var("CHELIX_E2E_SETUP_CODE")
                .unwrap_or_else(|_| auth::generate_setup_code());
            {
                let mut inner = state.inner.write().await;
                inner.setup_code = Some(Secret::new(code.clone()));
                inner.setup_code_created_at = Some(std::time::Instant::now());
            }
            Some(code)
        } else {
            None
        };

    let webauthn_registry = build_webauthn_registry(&config, port).await?;

    {
        let mut inner = state.inner.write().await;
        inner.heartbeat_config = config.heartbeat.clone();
        inner.channels_offered = config.channels.offered.clone();
    }
    #[cfg(feature = "graphql")]
    state.set_graphql_enabled(config.graphql.enabled);

    {
        let broadcaster: Arc<dyn chelix_tools::approval::ApprovalBroadcaster> =
            Arc::new(GatewayApprovalBroadcaster::new(Arc::clone(&state)));
        let scheme = if tls_enabled_for_gateway {
            "https"
        } else {
            "http"
        };
        let sandbox_gateway_url = Some(format!("{scheme}://host.docker.internal:{port}"));
        let sandbox_api_key = ensure_sandbox_api_key(&credential_store).await;

        let env_provider: Arc<dyn chelix_tools::command::EnvVarProvider> =
            Arc::new(CredentialEnvVarProvider {
                store: Arc::clone(&credential_store),
                gateway_url: sandbox_gateway_url,
                sandbox_api_key: sandbox_api_key.map(Secret::new),
            });
        let events_queue = cron_service.events_queue().clone();
        let cron_for_command_events = Arc::clone(&cron_service);
        let command_completion_callback: chelix_tools::command::CommandCompletionFn =
            Arc::new(move |event| {
                let summary = format!("Command `{}` exited {}", event.command, event.exit_code);
                let events_queue = Arc::clone(&events_queue);
                let cron_for_command_events = Arc::clone(&cron_for_command_events);
                tokio::spawn(async move {
                    events_queue
                        .enqueue(summary, chelix_cron::WAKE_REASON_COMMAND_EVENT.into())
                        .await;
                    cron_for_command_events
                        .wake(chelix_cron::WAKE_REASON_COMMAND_EVENT)
                        .await;
                });
            });

        let cron_tool = chelix_tools::cron_tool::CronTool::new(Arc::clone(&cron_service));

        let mut tool_registry = chelix_agents::tool_registry::ToolRegistry::new();
        let process_tool = chelix_tools::process::ProcessTool::new(Arc::clone(&sandbox_router));

        let sandbox_packages_tool =
            chelix_tools::sandbox_packages::SandboxPackagesTool::new(Arc::clone(&sandbox_router));

        let tmux_terminal_manager = Arc::new(chelix_tools::tmux_command::TmuxTerminalManager::new(
            Arc::clone(&sandbox_router),
            config.tools.execute_command.max_output_bytes,
        ));
        let mut execute_command_tool =
            chelix_tools::tmux_command::ExecuteCommandTool::new(Arc::clone(&tmux_terminal_manager))
                .with_default_timeout(std::time::Duration::from_secs(
                    config.tools.execute_command.default_timeout_secs,
                ))
                .with_approval(Arc::clone(&approval_manager), Arc::clone(&broadcaster))
                .with_env_provider(Arc::clone(&env_provider))
                .with_completion_callback(command_completion_callback);

        {
            let provider = Arc::new(crate::node_command::GatewayNodeCommandProvider::new(
                Arc::clone(&state),
                Arc::clone(&state.node_count),
                Arc::clone(&state.ssh_target_count),
                config.tools.execute_command.ssh_target.clone(),
                config.tools.execute_command.max_output_bytes,
            ));
            let default_node = match config.tools.execute_command.host.as_str() {
                "node" => config.tools.execute_command.node.clone(),
                "ssh" => config.tools.execute_command.ssh_target.clone(),
                _ => None,
            };
            execute_command_tool = execute_command_tool.with_node_provider(provider, default_node);
        }

        tool_registry.register(Box::new(execute_command_tool));
        tool_registry.register(Box::new(
            chelix_tools::tmux_command::ReadTerminalOutputTool::new(Arc::clone(
                &tmux_terminal_manager,
            )),
        ));
        tool_registry.register(Box::new(chelix_tools::calc::CalcTool::new()));
        tool_registry.register(Box::new(chelix_tools::ripgrep::RipgrepTool::new(
            Arc::clone(&tools_service),
        )));
        #[cfg(feature = "fs-tools")]
        {
            use chelix_config::schema::FsBinaryPolicy;
            let fs_cfg = &config.tools.fs;
            let path_policy = match chelix_tools::fs::FsPathPolicy::new(
                &fs_cfg.allow_paths,
                &fs_cfg.deny_paths,
            ) {
                Ok(p) => {
                    if p.is_empty() {
                        None
                    } else {
                        Some(p)
                    }
                },
                Err(e) => {
                    warn!(error = %e, "invalid tools.fs path policy — fs tools will run without path allow/deny");
                    None
                },
            };
            let workspace_root = fs_cfg.workspace_root.as_ref().map(PathBuf::from);
            let binary_policy = match fs_cfg.binary_policy {
                FsBinaryPolicy::Reject => chelix_tools::fs::BinaryPolicy::Reject,
                FsBinaryPolicy::Base64 => chelix_tools::fs::BinaryPolicy::Base64,
            };
            let shared_fs_state = if fs_cfg.track_reads {
                Some(chelix_tools::fs::new_fs_state(
                    fs_cfg.must_read_before_write,
                ))
            } else {
                None
            };
            let ctx = chelix_tools::fs::FsToolsContext {
                workspace_root,
                fs_state: shared_fs_state.clone(),
                path_policy,
                binary_policy,
                respect_gitignore: fs_cfg.respect_gitignore,
                sandbox_router: Arc::clone(&sandbox_router),
                approval_manager: fs_cfg
                    .require_approval
                    .then(|| Arc::clone(&approval_manager)),
                broadcaster: fs_cfg.require_approval.then(|| Arc::clone(&broadcaster)),
                max_read_bytes: Some(fs_cfg.max_read_bytes),
                context_window_tokens: fs_cfg.context_window_tokens,
            };
            chelix_tools::fs::register_fs_tools(&mut tool_registry, ctx);
        }
        #[cfg(feature = "wasm")]
        {
            let wasm_limits = sandbox_router
                .config()
                .wasm_tool_limits
                .clone()
                .unwrap_or_default();
            let epoch_interval_ms = sandbox_router
                .config()
                .wasm_epoch_interval_ms
                .unwrap_or(100);
            let brave_api_key = config
                .tools
                .web
                .search
                .api_key
                .as_ref()
                .map(|s| s.expose_secret().clone())
                .or_else(|| env_value_with_overrides(&runtime_env_overrides, "BRAVE_API_KEY"))
                .filter(|k| !k.trim().is_empty());
            if let Err(e) = chelix_tools::wasm_tool_runner::register_wasm_tools(
                &mut tool_registry,
                &wasm_limits,
                epoch_interval_ms,
                config.tools.web.fetch.timeout_seconds,
                config.tools.web.fetch.cache_ttl_minutes,
                config.tools.web.search.timeout_seconds,
                config.tools.web.search.cache_ttl_minutes,
                brave_api_key.as_deref(),
            ) {
                warn!(%e, "wasm tool registration failed");
            }
        }
        tool_registry.register(Box::new(process_tool));
        tool_registry.register(Box::new(sandbox_packages_tool));
        tool_registry.register(Box::new(cron_tool));
        tool_registry.register(Box::new(chelix_tools::webhook_tool::WebhookTool::new(
            Arc::clone(&state.services.webhooks),
        )));
        tool_registry.register(Box::new(crate::channel_agent_tools::SendMessageTool::new(
            Arc::clone(&state.services.channel),
        )));
        #[cfg(feature = "telephony")]
        crate::server::prepare_core::tool_registration::register_voice_call_tool(
            &mut tool_registry,
            &state,
        )
        .await;
        // MCP management tools — let agents add/remove/restart MCP servers directly.
        {
            let mcp = Arc::clone(&state.services.mcp);
            tool_registry.register(Box::new(crate::mcp_agent_tools::McpListTool::new(
                Arc::clone(&mcp),
            )));
            tool_registry.register(Box::new(crate::mcp_agent_tools::McpAddTool::new(
                Arc::clone(&mcp),
            )));
            tool_registry.register(Box::new(crate::mcp_agent_tools::McpRemoveTool::new(
                Arc::clone(&mcp),
            )));
            tool_registry.register(Box::new(crate::mcp_agent_tools::McpStatusTool::new(
                Arc::clone(&mcp),
            )));
            tool_registry.register(Box::new(crate::mcp_agent_tools::McpRestartTool::new(
                Arc::clone(&mcp),
            )));
        }
        #[cfg(feature = "msteams")]
        {
            let tp = Arc::clone(&msteams_webhook_plugin);
            tool_registry.register(Box::new(
                crate::teams_agent_tools::TeamsSearchMessagesTool::new(Arc::clone(&tp)),
            ));
            tool_registry.register(Box::new(
                crate::teams_agent_tools::TeamsMemberInfoTool::new(Arc::clone(&tp)),
            ));
            tool_registry.register(Box::new(
                crate::teams_agent_tools::TeamsPinMessageTool::new(Arc::clone(&tp)),
            ));
            tool_registry.register(Box::new(
                crate::teams_agent_tools::TeamsEditMessageTool::new(Arc::clone(&tp)),
            ));
            tool_registry.register(Box::new(
                crate::teams_agent_tools::TeamsReadMessageTool::new(Arc::clone(&tp)),
            ));
        }
        tool_registry.register(Box::new(
            crate::channel_agent_tools::UpdateChannelSettingsTool::new(
                Arc::clone(&state.services.channel),
                state.services.channel_store.clone(),
            ),
        ));
        tool_registry.register(Box::new(chelix_tools::send_image::SendImageTool::new(
            Arc::clone(&sandbox_router),
        )));
        #[cfg(feature = "provider-openai-codex")]
        if chelix_providers::openai_codex::has_stored_tokens() {
            tool_registry.register(Box::new(
                chelix_tools::image_generation::GenerateImageTool::new(),
            ));
        }
        tool_registry.register(Box::new(
            chelix_tools::send_document::SendDocumentTool::new(Arc::clone(&sandbox_router))
                .with_session_store(Arc::clone(&session_store)),
        ));
        if let Some(t) = chelix_tools::web_search::WebSearchTool::from_config_with_env_overrides(
            &config.tools.web.search,
            &runtime_env_overrides,
        ) {
            #[cfg(feature = "firecrawl")]
            let t = t.with_firecrawl_config(&config.tools.web.firecrawl);
            tool_registry.register(Box::new(t.with_env_provider(Arc::clone(&env_provider))));
        }
        if let Some(t) = chelix_tools::web_fetch::WebFetchTool::from_config(&config.tools.web.fetch)
        {
            #[cfg(feature = "firecrawl")]
            let t = t.with_firecrawl(&config.tools.web.firecrawl);
            tool_registry.register(Box::new(t));
        }
        #[cfg(feature = "firecrawl")]
        if let Some(t) =
            chelix_tools::firecrawl::FirecrawlScrapeTool::from_config(&config.tools.web.firecrawl)
        {
            tool_registry.register(Box::new(t));
        }
        if let Some(t) =
            chelix_tools::browser::BrowserTool::from_config(&config.tools.browser, &config.sandbox)
        {
            tool_registry.register(Box::new(t));
        }

        #[cfg(feature = "caldav")]
        {
            if let Some(t) = chelix_caldav::tool::CalDavTool::from_config(&config.caldav) {
                tool_registry.register(Box::new(t));
            }
        }

        #[cfg(feature = "home-assistant")]
        {
            if let Some(t) =
                chelix_home_assistant::tool::HomeAssistantTool::from_config(&config.home_assistant)
            {
                tool_registry.register(Box::new(t));
            }
        }

        if let Some(ref mm) = memory_manager {
            tool_registry.register(Box::new(chelix_memory::tools::MemorySearchTool::new(
                Arc::clone(mm),
            )));
            tool_registry.register(Box::new(chelix_memory::tools::MemoryGetTool::new(
                Arc::clone(mm),
            )));
            tool_registry.register(Box::new(chelix_memory::tools::MemorySaveTool::new(
                Arc::clone(mm),
            )));
            tool_registry.register(Box::new(chelix_memory::tools::MemoryDeleteTool::new(
                Arc::clone(mm),
            )));
            tool_registry.register(Box::new(chelix_chat::MemoryForgetTool::new(
                Arc::clone(mm),
                Arc::clone(&registry),
                Arc::clone(&session_metadata),
            )));
        }

        // ── Code index tools ─────────────────────────────────────────────
        #[cfg(feature = "qmd")]
        {
            use crate::project_aware_tools::ProjectAwareCodeIndexTool;
            chelix_code_index::tools::register_tools_wrapped(
                &mut tool_registry,
                code_index_for_tools,
                |tool| {
                    Box::new(ProjectAwareCodeIndexTool::new(
                        tool,
                        Arc::clone(&project_store),
                    ))
                },
            );
        }

        #[cfg(all(feature = "code-index-builtin", not(feature = "qmd")))]
        {
            use crate::project_aware_tools::ProjectAwareCodeIndexTool;
            chelix_code_index::tools::register_tools_wrapped(
                &mut tool_registry,
                code_index_for_tools_builtin,
                |tool| {
                    Box::new(ProjectAwareCodeIndexTool::new(
                        tool,
                        Arc::clone(&project_store),
                    ))
                },
            );
        }

        {
            let node_info_provider: Arc<dyn chelix_tools::nodes::NodeInfoProvider> =
                Arc::new(crate::node_command::GatewayNodeInfoProvider::new(
                    Arc::clone(&state),
                    config.tools.execute_command.ssh_target.clone(),
                ));
            tool_registry.register(Box::new(chelix_tools::nodes::NodesListTool::new(
                Arc::clone(&node_info_provider),
            )));
            tool_registry.register(Box::new(chelix_tools::nodes::NodesDescribeTool::new(
                Arc::clone(&node_info_provider),
            )));
            tool_registry.register(Box::new(chelix_tools::nodes::NodesSelectTool::new(
                Arc::clone(&node_info_provider),
            )));
        }

        tool_registry.register(Box::new(
            chelix_tools::session_state::SessionStateTool::new(Arc::clone(&session_state_store)),
        ));

        session_tools::register_session_tools(
            &mut tool_registry,
            &state,
            &session_store,
            &session_metadata,
        );

        tool_registry.register(Box::new(chelix_tools::task_list::TaskListTool::new(
            &data_dir,
        )));
        let mut speak_tool =
            crate::voice_agent_tools::SpeakTool::new(Arc::clone(&state.services.tts));
        if let Some(ref vps) = state.services.voice_persona_store {
            speak_tool = speak_tool.with_voice_persona_store(Arc::clone(vps));
        }
        tool_registry.register(Box::new(speak_tool));
        tool_registry.register(Box::new(crate::voice_agent_tools::TranscribeTool::new(
            Arc::clone(&state.services.stt),
        )));

        {
            use chelix_skills::{discover::FsSkillDiscoverer, usage::SkillUsageStore};

            let skill_usage = SkillUsageStore::open(&data_dir).await;

            tool_registry.register(Box::new(
                chelix_tools::skill_tools::CreateSkillTool::new(data_dir.clone())
                    .with_usage_store(skill_usage.clone()),
            ));
            tool_registry.register(Box::new(
                chelix_tools::skill_tools::UpdateSkillTool::new(data_dir.clone())
                    .with_usage_store(skill_usage.clone()),
            ));
            tool_registry.register(Box::new(
                chelix_tools::skill_tools::PatchSkillTool::new(data_dir.clone())
                    .with_usage_store(skill_usage.clone()),
            ));
            tool_registry.register(Box::new(
                chelix_tools::skill_tools::DeleteSkillTool::new(data_dir.clone())
                    .with_usage_store(skill_usage.clone()),
            ));

            let fs_discoverer =
                FsSkillDiscoverer::new(FsSkillDiscoverer::default_paths_for(&data_dir));

            #[cfg(feature = "bundled-skills")]
            {
                let bundled_store = Arc::new(chelix_skills::bundled::BundledSkillStore::new());
                let read_discoverer: Arc<dyn chelix_skills::discover::SkillDiscoverer> =
                    Arc::new(chelix_skills::discover::CompositeSkillDiscoverer::new(
                        Box::new(fs_discoverer),
                        Arc::clone(&bundled_store),
                    ));
                tool_registry.register(Box::new(
                    chelix_tools::skill_tools::ReadSkillTool::with_bundled(
                        read_discoverer,
                        bundled_store,
                    )
                    .with_usage_store(skill_usage.clone()),
                ));
            }
            #[cfg(not(feature = "bundled-skills"))]
            {
                let read_discoverer = Arc::new(fs_discoverer);
                tool_registry.register(Box::new(
                    chelix_tools::skill_tools::ReadSkillTool::new(read_discoverer)
                        .with_usage_store(skill_usage.clone()),
                ));
            }

            if config.skills.enable_agent_sidecar_files {
                tool_registry.register(Box::new(
                    chelix_tools::skill_tools::WriteSkillFilesTool::new(data_dir.clone()),
                ));
            }

            let _ = state.skill_usage_store.set(skill_usage);
        }

        tool_registry.register(Box::new(
            chelix_tools::branch_session::BranchSessionTool::new(
                Arc::clone(&session_store),
                Arc::clone(&session_metadata),
            ),
        ));

        let location_requester = Arc::new(crate::server::location::GatewayLocationRequester {
            state: Arc::clone(&state),
        });
        tool_registry.register(Box::new(chelix_tools::location::LocationTool::new(
            location_requester,
        )));

        let map_provider = match config.tools.maps.provider {
            chelix_config::schema::MapProvider::GoogleMaps => {
                chelix_tools::map::MapProvider::GoogleMaps
            },
            chelix_config::schema::MapProvider::AppleMaps => {
                chelix_tools::map::MapProvider::AppleMaps
            },
            chelix_config::schema::MapProvider::OpenStreetMap => {
                chelix_tools::map::MapProvider::OpenStreetMap
            },
        };
        tool_registry.register(Box::new(chelix_tools::map::ShowMapTool::with_provider(
            map_provider,
        )));

        if let Some(default_provider) = registry.read().await.first_with_tools() {
            let spawn_task_store =
                Arc::new(chelix_tools::spawn_agent_tasks::SpawnTaskStore::default());
            tool_registry.register(Box::new(
                chelix_tools::spawn_agent_tasks::SpawnStatusTool::new(Arc::clone(
                    &spawn_task_store,
                )),
            ));
            tool_registry.register(Box::new(
                chelix_tools::spawn_agent_tasks::SpawnResultTool::new(Arc::clone(
                    &spawn_task_store,
                )),
            ));
            tool_registry.register(Box::new(
                chelix_tools::spawn_agent_tasks::SpawnListTool::new(Arc::clone(&spawn_task_store)),
            ));
            tool_registry.register(Box::new(
                chelix_tools::spawn_agent_tasks::SpawnCancelTool::new(Arc::clone(
                    &spawn_task_store,
                )),
            ));
            let base_tools = Arc::new(tool_registry.clone_without(&[]));
            let state_for_spawn = Arc::clone(&state);
            let on_spawn_event: chelix_tools::spawn_agent::OnSpawnEvent = Arc::new(move |event| {
                use chelix_agents::runner::RunnerEvent;
                let state = Arc::clone(&state_for_spawn);
                let payload = match &event {
                    RunnerEvent::SubAgentStart { task, model, depth } => {
                        serde_json::json!({
                            "state": "sub_agent_start",
                            "task": task,
                            "model": model,
                            "depth": depth,
                        })
                    },
                    RunnerEvent::SubAgentEnd {
                        task,
                        model,
                        depth,
                        iterations,
                        tool_calls_made,
                    } => serde_json::json!({
                        "state": "sub_agent_end",
                        "task": task,
                        "model": model,
                        "depth": depth,
                        "iterations": iterations,
                        "toolCallsMade": tool_calls_made,
                    }),
                    _ => return,
                };
                tokio::spawn(async move {
                    broadcast(&state, "chat", payload, BroadcastOpts::default()).await;
                });
            });
            let spawn_tool = chelix_tools::spawn_agent::SpawnAgentTool::new(
                Arc::clone(&registry),
                default_provider,
                base_tools,
            )
            .with_on_event(on_spawn_event)
            .with_agents_config(agents_config)
            .with_task_store(Arc::clone(&spawn_task_store));
            tool_registry.register(Box::new(spawn_tool));
        }

        let shared_tool_registry = Arc::new(tokio::sync::RwLock::new(tool_registry));
        let mut chat_service = LiveChatService::new(
            Arc::clone(&registry),
            Arc::clone(&model_store),
            crate::chat::GatewayChatRuntime::from_state(Arc::clone(&state)),
            Arc::clone(&session_store),
            Arc::clone(&session_metadata),
        )
        .with_session_state_store(Arc::clone(&session_state_store))
        .with_tools(Arc::clone(&shared_tool_registry))
        .with_session_mutations(Arc::clone(&session_mutations))
        .with_config(config.clone());

        if let Some(ref hooks) = state.inner.read().await.hook_registry {
            chat_service = chat_service.with_hooks_arc(Arc::clone(hooks));
        }

        let live_chat = Arc::new(chat_service);
        let chat_with_external_agents = Arc::new(ExternalAgentChatService::new(
            live_chat,
            external_agent_service,
            Arc::clone(&state),
            Arc::clone(&session_store),
            Arc::clone(&session_metadata),
        ));
        state.set_chat(chat_with_external_agents);

        live_mcp
            .set_tool_registry(Arc::clone(&shared_tool_registry))
            .await;
        crate::mcp_service::sync_mcp_tools(live_mcp.manager(), &shared_tool_registry).await;

        let schemas = shared_tool_registry.read().await.list_schemas();
        let tool_names: Vec<&str> = schemas.iter().filter_map(|s| s["name"].as_str()).collect();
        info!(tools = ?tool_names, "agent tools registered");
    }

    #[cfg(feature = "file-watcher")]
    {
        let watcher_state = Arc::clone(&state);
        tokio::spawn(async move {
            let (mut watcher, mut rx) = match start_skill_hot_reload_watcher().await {
                Ok(started) => started,
                Err(error) => {
                    tracing::warn!("skills: failed to start file watcher: {error}");
                    return;
                },
            };

            loop {
                let Some(event) = rx.recv().await else {
                    break;
                };
                broadcast(
                    &watcher_state,
                    "skills.changed",
                    serde_json::json!({}),
                    BroadcastOpts::default(),
                )
                .await;

                if matches!(
                    event,
                    chelix_skills::watcher::SkillWatchEvent::ManifestChanged
                ) {
                    match start_skill_hot_reload_watcher().await {
                        Ok((new_watcher, new_rx)) => {
                            watcher = new_watcher;
                            rx = new_rx;
                        },
                        Err(error) => {
                            tracing::warn!("skills: failed to refresh file watcher: {error}");
                        },
                    }
                }
            }

            drop(watcher);
        });
    }

    {
        let health_state = Arc::clone(&state);
        let health_mcp = Arc::clone(&live_mcp);
        tokio::spawn(async move {
            crate::mcp_health::run_health_monitor(health_state, health_mcp).await;
        });
    }

    let methods = Arc::new(MethodRegistry::new());

    #[cfg(feature = "push-notifications")]
    let push_service: Option<Arc<crate::push::PushService>> = {
        match crate::push::PushService::new(&data_dir).await {
            Ok(svc) => {
                info!("push notification service initialized");
                state.set_push_service(Arc::clone(&svc)).await;
                Some(svc)
            },
            Err(e) => {
                tracing::warn!("failed to initialize push notification service: {e}");
                None
            },
        }
    };

    startup_mem_probe.checkpoint("prepare_gateway.ready");

    Ok(PreparedGatewayCore {
        state: Arc::clone(&state),
        methods: Arc::clone(&methods),
        webauthn_registry,
        #[cfg(feature = "msteams")]
        msteams_webhook_plugin,
        #[cfg(feature = "slack")]
        slack_webhook_plugin,
        #[cfg(feature = "telephony")]
        telephony_webhook_plugin,
        #[cfg(feature = "push-notifications")]
        push_service,
        sandbox_router,
        browser_for_lifecycle,
        cron_service,
        log_buffer,
        config,
        data_dir,
        provider_summary,
        mcp_configured_count,
        setup_code_display,
        port,
        tls_enabled: tls_enabled_for_gateway,
        browser_tool_for_warmup: None,
    })
}
