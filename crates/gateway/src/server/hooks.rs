use std::{collections::HashSet, sync::Arc};

use tracing::{info, warn};

use chelix_sessions::store::SessionStore;

use super::seed_content::{EXAMPLE_HOOK_MD, EXAMPLE_SKILL_MD, TMUX_SKILL_MD};

// ── Hook seeding helpers ─────────────────────────────────────────────────────

/// Seed a skeleton example hook into `~/.chelix/hooks/example/` on first run.
pub(crate) fn seed_example_hook() {
    let hook_dir = chelix_config::data_dir().join("hooks/example");
    let hook_md = hook_dir.join("HOOK.md");
    if hook_md.exists() {
        return;
    }
    if let Err(e) = std::fs::create_dir_all(&hook_dir) {
        tracing::debug!("could not create example hook dir: {e}");
        return;
    }
    if let Err(e) = std::fs::write(&hook_md, EXAMPLE_HOOK_MD) {
        tracing::debug!("could not write example HOOK.md: {e}");
    }
}

/// Seed built-in personal skills into `~/.chelix/skills/`.
pub(crate) fn seed_example_skill() {
    seed_skill_if_missing("template-skill", EXAMPLE_SKILL_MD);
    seed_skill_if_missing("tmux", TMUX_SKILL_MD);
}

fn seed_skill_if_missing(name: &str, content: &str) {
    let skill_dir = chelix_config::data_dir().join(format!("skills/{name}"));
    let skill_md = skill_dir.join("SKILL.md");
    if skill_md.exists() {
        return;
    }
    if let Err(e) = std::fs::create_dir_all(&skill_dir) {
        tracing::debug!("could not create {name} skill dir: {e}");
        return;
    }
    if let Err(e) = std::fs::write(&skill_md, content) {
        tracing::debug!("could not write {name} SKILL.md: {e}");
    }
}

// ── Hook discovery ───────────────────────────────────────────────────────────

/// Metadata for built-in hooks (compiled Rust, always active).
fn builtin_hook_metadata() -> Vec<(
    &'static str,
    &'static str,
    Vec<chelix_common::hooks::HookEvent>,
    &'static str,
)> {
    use chelix_common::hooks::HookEvent;
    vec![
        (
            "command-logger",
            "Logs all slash-command invocations to a JSONL audit file at ~/.chelix/logs/commands.log.",
            vec![HookEvent::Command],
            "crates/plugins/src/bundled/command_logger.rs",
        ),
        (
            "session-memory",
            "Saves the conversation history to a markdown file in the memory directory when a session is reset or a new session is created, making it searchable for future sessions.",
            vec![HookEvent::Command],
            "crates/plugins/src/bundled/session_memory.rs",
        ),
    ]
}

/// Discover hooks from the filesystem, check eligibility, and build a
/// [`HookRegistry`] plus a `Vec<DiscoveredHookInfo>` for the web UI.
pub(crate) async fn discover_and_build_hooks(
    disabled: &HashSet<String>,
    session_store: Option<&Arc<SessionStore>>,
) -> anyhow::Result<(
    Option<Arc<chelix_common::hooks::HookRegistry>>,
    Vec<crate::state::DiscoveredHookInfo>,
)> {
    use chelix_plugins::{
        bundled::{command_logger::CommandLoggerHook, session_memory::SessionMemoryHook},
        hook_discovery::{FsHookDiscoverer, HookDiscoverer, HookSource},
        hook_eligibility::check_hook_eligibility,
        shell_hook::ShellHookHandler,
    };

    let config = chelix_config::discover_and_load()?;
    let discoverer = FsHookDiscoverer::new(FsHookDiscoverer::default_paths());
    let discovered = discoverer.discover().await.unwrap_or_default();
    let session_export_mode = config.memory.session_export;

    let mut registry = chelix_common::hooks::HookRegistry::new();
    let mut info_list = Vec::with_capacity(discovered.len());

    for (parsed, source) in &discovered {
        let meta = &parsed.metadata;
        let elig = check_hook_eligibility(meta);
        let is_disabled = disabled.contains(&meta.name);
        let is_enabled = elig.eligible && !is_disabled;

        if !elig.eligible {
            info!(
                hook = %meta.name,
                source = ?source,
                missing_os = elig.missing_os,
                missing_bins = ?elig.missing_bins,
                missing_env = ?elig.missing_env,
                "hook ineligible, skipping"
            );
        }

        let raw_content =
            std::fs::read_to_string(parsed.source_path.join("HOOK.md")).unwrap_or_default();

        let source_str = match source {
            HookSource::Project => "project",
            HookSource::User => "user",
            HookSource::Bundled => "bundled",
        };

        info_list.push(crate::state::DiscoveredHookInfo {
            name: meta.name.clone(),
            description: meta.description.clone(),
            emoji: meta.emoji.clone(),
            events: meta.events.iter().map(|e| e.to_string()).collect(),
            command: meta.command.clone(),
            timeout: meta.timeout,
            priority: meta.priority,
            source: source_str.to_string(),
            source_path: parsed.source_path.display().to_string(),
            eligible: elig.eligible,
            missing_os: elig.missing_os,
            missing_bins: elig.missing_bins.clone(),
            missing_env: elig.missing_env.clone(),
            enabled: is_enabled,
            body: raw_content,
            body_html: crate::services::markdown_to_html(&parsed.body),
            call_count: 0,
            failure_count: 0,
            avg_latency_ms: 0,
        });

        if is_enabled && let Some(ref command) = meta.command {
            let handler = ShellHookHandler::new(
                meta.name.clone(),
                command.clone(),
                meta.events.clone(),
                std::time::Duration::from_secs(meta.timeout),
                meta.env.clone(),
                Some(parsed.source_path.clone()),
            );
            registry.register(Arc::new(handler));
        }
    }

    let filesystem_hook_names = discovered
        .iter()
        .map(|(parsed, _source)| parsed.metadata.name.as_str())
        .collect::<HashSet<_>>();
    let mut config_hook_count = 0;
    let config_path = chelix_config::config_dir()
        .map(|path| path.join("chelix.toml").display().to_string())
        .unwrap_or_else(|| "chelix.toml".to_string());
    let mut config_hook_names = HashSet::new();

    if let Some(hooks_config) = config.hooks.as_ref() {
        for hook in &hooks_config.hooks {
            if filesystem_hook_names.contains(hook.name.as_str()) {
                warn!(
                    hook = %hook.name,
                    "config hook conflicts with filesystem hook; keeping filesystem hook"
                );
                continue;
            }

            if !config_hook_names.insert(hook.name.as_str()) {
                warn!(
                    hook = %hook.name,
                    "duplicate config hook name; keeping first config hook"
                );
                continue;
            }

            let events = hook
                .events
                .iter()
                .filter_map(|event| match event.parse::<chelix_common::hooks::HookEvent>() {
                    Ok(event) => Some(event),
                    Err(e) => {
                        warn!(hook = %hook.name, event = %event, error = %e, "skipping invalid config hook event");
                        None
                    },
                })
                .collect::<Vec<_>>();
            let event_names = events.iter().map(|event| event.to_string()).collect();
            let is_enabled = !disabled.contains(&hook.name) && !events.is_empty();
            config_hook_count += 1;

            info_list.push(crate::state::DiscoveredHookInfo {
                name: hook.name.clone(),
                description: String::new(),
                emoji: None,
                events: event_names,
                command: Some(hook.command.clone()),
                timeout: hook.timeout,
                priority: 0,
                source: "config".to_string(),
                source_path: config_path.clone(),
                eligible: !events.is_empty(),
                missing_os: false,
                missing_bins: vec![],
                missing_env: vec![],
                enabled: is_enabled,
                body: String::new(),
                body_html: "<p><em>Shell hook declared in chelix.toml.</em></p>".to_string(),
                call_count: 0,
                failure_count: 0,
                avg_latency_ms: 0,
            });

            if is_enabled {
                let handler = ShellHookHandler::new(
                    hook.name.clone(),
                    hook.command.clone(),
                    events,
                    std::time::Duration::from_secs(hook.timeout),
                    hook.env.clone(),
                    None,
                );
                registry.register(Arc::new(handler));
            }
        }
    }

    // ── Built-in hooks (compiled Rust, always active) ──────────────────
    {
        let data = chelix_config::data_dir();

        let log_path =
            CommandLoggerHook::default_path().unwrap_or_else(|| data.join("logs/commands.log"));
        let logger = CommandLoggerHook::new(log_path);
        registry.register(Arc::new(logger));

        if let Some(store) = session_store
            && !matches!(session_export_mode, chelix_config::SessionExportMode::Off)
        {
            let memory_hook = SessionMemoryHook::new(data.clone(), Arc::clone(store));
            registry.register(Arc::new(memory_hook));
        }
    }

    for (name, description, events, source_file) in builtin_hook_metadata() {
        let enabled = if name == "session-memory" {
            !matches!(session_export_mode, chelix_config::SessionExportMode::Off)
        } else {
            true
        };
        info_list.push(crate::state::DiscoveredHookInfo {
            name: name.to_string(),
            description: description.to_string(),
            emoji: Some("\u{2699}\u{fe0f}".to_string()),
            events: events.iter().map(|e| e.to_string()).collect(),
            command: None,
            timeout: 0,
            priority: 0,
            source: "builtin".to_string(),
            source_path: source_file.to_string(),
            eligible: true,
            missing_os: false,
            missing_bins: vec![],
            missing_env: vec![],
            enabled,
            body: String::new(),
            body_html: format!(
                "<p><em>Built-in hook implemented in Rust.</em></p><p>{}</p>",
                description
            ),
            call_count: 0,
            failure_count: 0,
            avg_latency_ms: 0,
        });
    }

    if !info_list.is_empty() {
        info!(
            "{} hook(s) discovered ({} filesystem shell, {} config shell, {} built-in), {} registered",
            info_list.len(),
            discovered.len(),
            config_hook_count,
            info_list.len() - discovered.len() - config_hook_count,
            registry.handler_names().len()
        );
    }

    Ok((Some(Arc::new(registry)), info_list))
}
