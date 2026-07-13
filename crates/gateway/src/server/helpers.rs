use std::{
    collections::HashMap,
    fs::OpenOptions,
    io::Write,
    path::{Path as FsPath, PathBuf},
    sync::Arc,
};

use tracing::{debug, info, warn};

use chelix_providers::ProviderRegistry;

use chelix_tools::approval::{ApprovalManager, ApprovalMode, SecurityLevel};

// ── QMD helpers ──────────────────────────────────────────────────────────────

#[cfg(feature = "qmd")]
pub(crate) fn sanitize_qmd_index_name(root: &FsPath) -> String {
    let mut sanitized = String::new();
    let mut previous_was_separator = false;
    for character in root.to_string_lossy().chars() {
        let normalized = character.to_ascii_lowercase();
        if normalized.is_ascii_alphanumeric() {
            sanitized.push(normalized);
            previous_was_separator = false;
        } else if !previous_was_separator {
            sanitized.push('_');
            previous_was_separator = true;
        }
    }
    let sanitized = sanitized.trim_matches('_').to_string();
    if sanitized.is_empty() {
        "chelix".into()
    } else {
        format!("chelix-{sanitized}")
    }
}

#[cfg(feature = "qmd")]
pub(crate) fn build_qmd_collections(
    data_dir: &FsPath,
    config: &chelix_config::schema::QmdConfig,
) -> HashMap<String, chelix_qmd::QmdCollection> {
    if config.collections.is_empty() {
        return HashMap::from([
            ("chelix-root-memory".into(), chelix_qmd::QmdCollection {
                path: data_dir.to_path_buf(),
                glob: "MEMORY.md".into(),
            }),
            (
                "chelix-root-memory-lower".into(),
                chelix_qmd::QmdCollection {
                    path: data_dir.to_path_buf(),
                    glob: "memory.md".into(),
                },
            ),
            ("chelix-memory".into(), chelix_qmd::QmdCollection {
                path: data_dir.join("memory"),
                glob: "**/*.md".into(),
            }),
            ("chelix-agents".into(), chelix_qmd::QmdCollection {
                path: data_dir.join("agents"),
                glob: "**/*.md".into(),
            }),
        ]);
    }

    let mut collections = HashMap::new();
    for (name, collection) in &config.collections {
        let globs = if collection.globs.is_empty() {
            vec!["**/*.md".to_string()]
        } else {
            collection.globs.clone()
        };

        for (path_index, path) in collection.paths.iter().enumerate() {
            let root = FsPath::new(path);
            let root = if root.is_absolute() {
                root.to_path_buf()
            } else {
                data_dir.join(root)
            };

            for (glob_index, glob) in globs.iter().enumerate() {
                let key = if collection.paths.len() == 1 && globs.len() == 1 {
                    name.clone()
                } else {
                    format!("{name}-{path_index}-{glob_index}")
                };
                collections.insert(key, chelix_qmd::QmdCollection {
                    path: root.clone(),
                    glob: glob.clone(),
                });
            }
        }
    }

    collections
}

// ── Sandbox helpers ──────────────────────────────────────────────────────────

pub(crate) fn should_prebuild_sandbox_image(
    mode: &chelix_tools::sandbox::SandboxMode,
    packages: &[String],
) -> bool {
    !matches!(mode, chelix_tools::sandbox::SandboxMode::Off) && !packages.is_empty()
}

pub(crate) fn instance_slug(config: &chelix_config::ChelixConfig) -> String {
    let mut raw_name = config.identity.name.clone();
    if let Some(file_identity) = chelix_config::load_identity_for_agent("main")
        && file_identity.name.is_some()
    {
        raw_name = file_identity.name;
    }

    let base = raw_name
        .unwrap_or_else(|| "chelix".to_string())
        .to_lowercase();
    let mut out = String::new();
    let mut last_dash = false;
    for ch in base.chars() {
        let mapped = if ch.is_ascii_alphanumeric() {
            ch
        } else {
            '-'
        };
        if mapped == '-' {
            if !last_dash {
                out.push(mapped);
            }
            last_dash = true;
        } else {
            out.push(mapped);
            last_dash = false;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "chelix".to_string()
    } else {
        out
    }
}

pub(crate) fn sandbox_container_prefix(instance_slug: &str) -> String {
    format!("chelix-{instance_slug}-sandbox")
}

pub(crate) fn browser_container_prefix(instance_slug: &str) -> String {
    format!("chelix-{instance_slug}-browser")
}

// ── Environment helpers ──────────────────────────────────────────────────────

pub(crate) fn env_value_with_overrides(
    env_overrides: &HashMap<String, String>,
    key: &str,
) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            env_overrides
                .get(key)
                .cloned()
                .filter(|value| !value.trim().is_empty())
        })
}

pub(crate) fn env_var_or_unset(name: &str) -> String {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "<unset>".to_string())
}

pub(crate) fn env_flag_enabled(name: &str) -> bool {
    std::env::var(name)
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

// ── Model inventory logging ──────────────────────────────────────────────────

pub(crate) fn summarize_model_ids_for_logs(
    sorted_model_ids: &[String],
    max_items: usize,
) -> Vec<String> {
    if max_items == 0 {
        return Vec::new();
    }

    if sorted_model_ids.len() <= max_items || max_items < 3 {
        return sorted_model_ids.iter().take(max_items).cloned().collect();
    }

    let head_count = max_items / 2;
    let tail_count = max_items - head_count - 1;
    let mut sample = Vec::with_capacity(max_items);
    sample.extend(sorted_model_ids.iter().take(head_count).cloned());
    sample.push("...".to_string());
    sample.extend(
        sorted_model_ids
            .iter()
            .skip(sorted_model_ids.len().saturating_sub(tail_count))
            .cloned(),
    );
    sample
}

pub(crate) fn log_startup_model_inventory(reg: &ProviderRegistry) {
    const STARTUP_MODEL_SAMPLE_SIZE: usize = 8;
    const STARTUP_PROVIDER_MODEL_SAMPLE_SIZE: usize = 4;

    let mut by_provider: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    let mut model_ids: Vec<String> = Vec::with_capacity(reg.list_models().len());
    for model in reg.list_models() {
        model_ids.push(model.id.clone());
        by_provider
            .entry(model.provider.clone())
            .or_default()
            .push(model.id.clone());
    }
    model_ids.sort();

    let provider_model_counts: Vec<(String, usize)> = by_provider
        .iter()
        .map(|(provider, provider_models)| (provider.clone(), provider_models.len()))
        .collect();

    info!(
        model_count = model_ids.len(),
        provider_count = by_provider.len(),
        provider_model_counts = ?provider_model_counts,
        sample_model_ids = ?summarize_model_ids_for_logs(&model_ids, STARTUP_MODEL_SAMPLE_SIZE),
        "startup model inventory"
    );

    for (provider, provider_models) in &mut by_provider {
        provider_models.sort();
        debug!(
            provider = %provider,
            model_count = provider_models.len(),
            sample_model_ids = ?summarize_model_ids_for_logs(
                provider_models,
                STARTUP_PROVIDER_MODEL_SAMPLE_SIZE
            ),
            "startup provider model inventory"
        );
    }
}

// ── Approval manager ─────────────────────────────────────────────────────────

pub fn approval_manager_from_config(config: &chelix_config::ChelixConfig) -> ApprovalManager {
    let mut manager = ApprovalManager::default();

    manager.mode =
        ApprovalMode::parse(&config.tools.execute_command.approval_mode).unwrap_or_else(|| {
            warn!(
                value = %config.tools.execute_command.approval_mode,
                "invalid tools.execute_command.approval_mode; falling back to 'on-miss'"
            );
            ApprovalMode::OnMiss
        });

    manager.security_level = SecurityLevel::parse(&config.tools.execute_command.security_level)
        .unwrap_or_else(|| {
            warn!(
                value = %config.tools.execute_command.security_level,
                "invalid tools.execute_command.security_level; falling back to 'allowlist'"
            );
            SecurityLevel::Allowlist
        });

    manager.allowlist = config.tools.execute_command.allowlist.clone();
    manager
}

// ── Memory / process diagnostics ─────────────────────────────────────────────

pub(crate) fn process_rss_bytes() -> u64 {
    let mut sys = sysinfo::System::new();
    let Some(pid) = sysinfo::get_current_pid().ok() else {
        return 0;
    };
    sys.refresh_memory();
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[pid]),
        false,
        sysinfo::ProcessRefreshKind::nothing().with_memory(),
    );
    sys.process(pid).map(|p| p.memory()).unwrap_or(0)
}

pub(crate) struct StartupMemProbe {
    enabled: bool,
    last_rss_bytes: u64,
}

impl StartupMemProbe {
    pub(crate) fn new() -> Self {
        let enabled = env_flag_enabled("CHELIX_STARTUP_MEM_TRACE");
        let last_rss_bytes = if enabled {
            process_rss_bytes()
        } else {
            0
        };
        Self {
            enabled,
            last_rss_bytes,
        }
    }

    pub(crate) fn checkpoint(&mut self, stage: &str) {
        if !self.enabled {
            return;
        }
        let rss_bytes = process_rss_bytes();
        let delta_bytes = rss_bytes as i128 - self.last_rss_bytes as i128;
        self.last_rss_bytes = rss_bytes;

        info!(
            stage,
            rss_bytes,
            delta_bytes = delta_bytes as i64,
            "startup memory checkpoint"
        );
    }
}

// ── TLS / proxy validation ───────────────────────────────────────────────────

pub(crate) fn validate_proxy_tls_configuration(
    behind_proxy: bool,
    tls_enabled: bool,
    allow_tls_behind_proxy: bool,
) -> anyhow::Result<()> {
    if behind_proxy && tls_enabled && !allow_tls_behind_proxy {
        anyhow::bail!(
            "CHELIX_BEHIND_PROXY=true with Chelix TLS enabled is usually a proxy misconfiguration. Run with --no-tls (or CHELIX_NO_TLS=true). If your proxy upstream is HTTPS/TCP passthrough by design, set CHELIX_ALLOW_TLS_BEHIND_PROXY=true."
        );
    }
    Ok(())
}

// ── Path / storage diagnostics ───────────────────────────────────────────────

pub(crate) fn log_path_diagnostics(kind: &str, path: &FsPath) {
    match std::fs::metadata(path) {
        Ok(metadata) => {
            info!(
                kind,
                path = %path.display(),
                exists = true,
                is_dir = metadata.is_dir(),
                readonly = metadata.permissions().readonly(),
                size_bytes = metadata.len(),
                "startup path diagnostics"
            );
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            info!(kind, path = %path.display(), exists = false, "startup path missing");
        },
        Err(error) => {
            warn!(
                kind,
                path = %path.display(),
                error = %error,
                "failed to inspect startup path"
            );
        },
    }
}

pub(crate) fn log_directory_write_probe(dir: &FsPath) {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let probe_path = dir.join(format!(
        ".chelix-write-check-{}-{nanos}.tmp",
        std::process::id()
    ));

    match OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&probe_path)
    {
        Ok(mut file) => {
            if let Err(error) = file.write_all(b"probe") {
                warn!(
                    path = %probe_path.display(),
                    error = %error,
                    "startup write probe could not write to config directory"
                );
            } else {
                info!(
                    path = %probe_path.display(),
                    "startup write probe succeeded for config directory"
                );
            }
            if let Err(error) = std::fs::remove_file(&probe_path) {
                warn!(
                    path = %probe_path.display(),
                    error = %error,
                    "failed to clean up startup write probe file"
                );
            }
        },
        Err(error) => {
            warn!(
                path = %probe_path.display(),
                error = %error,
                "startup write probe failed for config directory"
            );
        },
    }
}

// ── Config / storage startup logging ─────────────────────────────────────────

pub(crate) fn log_startup_config_storage_diagnostics() {
    let config_dir = chelix_config::config_dir().unwrap_or_else(|| PathBuf::from(".chelix"));
    let discovered_config = chelix_config::loader::find_config_file();
    let expected_config = chelix_config::find_or_default_config_path();
    let provider_keys_path = config_dir.join("provider_keys.json");

    let discovered_display = discovered_config
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "<none>".to_string());
    info!(
        user = %env_var_or_unset("USER"),
        home = %env_var_or_unset("HOME"),
        config_dir = %config_dir.display(),
        discovered_config = %discovered_display,
        expected_config = %expected_config.display(),
        provider_keys_path = %provider_keys_path.display(),
        "startup configuration storage diagnostics"
    );

    log_path_diagnostics("config-dir", &config_dir);
    log_directory_write_probe(&config_dir);

    if let Some(path) = discovered_config {
        log_path_diagnostics("config-file", &path);
    } else if expected_config.exists() {
        info!(
            path = %expected_config.display(),
            "default config file exists even though discovery did not report a named config"
        );
        log_path_diagnostics("config-file", &expected_config);
    } else {
        warn!(
            path = %expected_config.display(),
            "no config file detected on startup; Chelix is running with in-memory defaults until config is persisted"
        );
    }

    if provider_keys_path.exists() {
        log_path_diagnostics("provider-keys", &provider_keys_path);
        match std::fs::read_to_string(&provider_keys_path) {
            Ok(content) => match serde_json::from_str::<serde_json::Value>(&content) {
                Ok(_) => {
                    info!(
                        path = %provider_keys_path.display(),
                        bytes = content.len(),
                        "provider key store file is readable JSON"
                    );
                },
                Err(error) => {
                    warn!(
                        path = %provider_keys_path.display(),
                        error = %error,
                        "provider key store file contains invalid JSON"
                    );
                },
            },
            Err(error) => {
                warn!(
                    path = %provider_keys_path.display(),
                    error = %error,
                    "provider key store file exists but is not readable"
                );
            },
        }
    } else {
        info!(
            path = %provider_keys_path.display(),
            "provider key store file not found yet; it will be created after the first providers.save_key"
        );
    }
}

// ── Cron delivery ────────────────────────────────────────────────────────────

pub(crate) async fn maybe_deliver_cron_output(
    outbound: Option<Arc<dyn chelix_channels::ChannelOutbound>>,
    req: &chelix_cron::service::AgentTurnRequest,
    delivery_text: &str,
) {
    if !req.deliver || delivery_text.trim().is_empty() {
        return;
    }

    let (Some(channel_account), Some(chat_id)) = (&req.channel, &req.to) else {
        return;
    };

    if let Some(outbound) = outbound {
        if let Err(error) = outbound
            .send_text(channel_account, chat_id, delivery_text, None)
            .await
        {
            tracing::warn!(
                channel = %channel_account,
                to = %chat_id,
                error = %error,
                "cron job channel delivery failed"
            );
        }
    } else {
        tracing::debug!("cron job delivery requested but no channel outbound configured");
    }
}

// ── Skill hot-reload watcher ─────────────────────────────────────────────────

#[cfg(feature = "file-watcher")]
pub(crate) async fn start_skill_hot_reload_watcher() -> anyhow::Result<(
    chelix_skills::watcher::SkillWatcher,
    tokio::sync::mpsc::UnboundedReceiver<chelix_skills::watcher::SkillWatchEvent>,
)> {
    let watch_specs = tokio::task::spawn_blocking(chelix_skills::watcher::default_watch_specs)
        .await
        .map_err(|error| anyhow::anyhow!("skills watcher task failed: {error}"))??;

    Ok(chelix_skills::watcher::SkillWatcher::start(watch_specs)?)
}
