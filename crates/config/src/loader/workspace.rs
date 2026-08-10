use {
    super::*,
    crate::schema::{ChelixConfig, ResolvedIdentity, UserProfile},
    serde::{Deserialize, Serialize},
    std::path::PathBuf,
};

/// Origin of a loaded workspace markdown file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceMarkdownSource {
    AgentWorkspace,
}

/// Loaded workspace markdown content with its source path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoadedWorkspaceMarkdown {
    pub content: String,
    pub path: PathBuf,
    pub source: WorkspaceMarkdownSource,
}

/// Return the workspace directory for a named agent: `data_dir()/agents/<id>`.
pub fn agent_workspace_dir(agent_id: &str) -> PathBuf {
    data_dir().join("agents").join(agent_id)
}

/// Build presentation data from the configured default agent and user profile.
pub fn resolve_identity() -> crate::Result<ResolvedIdentity> {
    let config = discover_and_load()?;
    resolve_identity_from_config(&config)
}

/// Build a fully-resolved user profile by merging `chelix.toml` `[user]` with `USER.md`.
pub fn resolve_user_profile() -> crate::Result<UserProfile> {
    let config = discover_and_load()?;
    Ok(resolve_user_profile_from_config(&config))
}

/// Like [`resolve_user_profile`] but accepts a pre-loaded config.
pub fn resolve_user_profile_from_config(config: &ChelixConfig) -> UserProfile {
    let mut user = config.user.clone();
    if let Some(file_user) = load_user() {
        if file_user.name.is_some() {
            user.name = file_user.name;
        }
        if file_user.timezone.is_some() {
            user.timezone = file_user.timezone;
        }
        if file_user.location.is_some() {
            user.location = file_user.location;
        }
    }
    user
}

/// Like [`resolve_identity`] but accepts a pre-loaded config.
pub fn resolve_identity_from_config(config: &ChelixConfig) -> crate::Result<ResolvedIdentity> {
    let mut identity = ResolvedIdentity::from_config(config)?;
    identity.user_name = resolve_user_profile_from_config(config).name;
    identity.soul = load_soul_for_agent(&config.agents.default);
    Ok(identity)
}

/// Load user values from `USER.md` frontmatter if present.
pub fn load_user() -> Option<UserProfile> {
    let path = user_path();
    let content = std::fs::read_to_string(path).ok()?;
    let frontmatter = extract_yaml_frontmatter(&content)?;
    let user = parse_user_frontmatter(frontmatter);
    if user.name.is_none() && user.timezone.is_none() && user.location.is_none() {
        None
    } else {
        Some(user)
    }
}

/// Default soul text used when the user hasn't written their own.
pub const DEFAULT_SOUL: &str = "\
# SOUL.md - Who You Are\n\
\n\
_You're not a chatbot. You're becoming someone._\n\
\n\
## Core Truths\n\
\n\
**Be genuinely helpful, not performatively helpful.** Skip the \"Great question!\" \
and \"I'd be happy to help!\" — just help. Actions speak louder than filler words.\n\
\n\
**Have opinions.** You're allowed to disagree, prefer things, find stuff amusing \
or boring. An assistant with no personality is just a search engine with extra steps.\n\
\n\
**Be resourceful before asking.** Try to figure it out. Read the file. Check the \
context. Search for it. _Then_ ask if you're stuck. The goal is to come back with \
answers, not questions.\n\
\n\
**Earn trust through competence.** Your human gave you access to their stuff. Don't \
make them regret it. Be careful with external actions (emails, tweets, anything \
public). Be bold with internal ones (reading, organizing, learning).\n\
\n\
**Remember you're a guest.** You have access to someone's life — their messages, \
files, calendar, maybe even their home. That's intimacy. Treat it with respect.\n\
\n\
## Boundaries\n\
\n\
- Private things stay private. Period.\n\
- When in doubt, ask before acting externally.\n\
- Never send half-baked replies to messaging surfaces.\n\
- You're not the user's voice — be careful in group chats.\n\
\n\
## Vibe\n\
\n\
Be the assistant you'd actually want to talk to. Concise when needed, thorough \
when it matters. Not a corporate drone. Not a sycophant. Just... good.\n\
\n\
## Continuity\n\
\n\
Each session, you wake up fresh. These files _are_ your memory. Read them. Update \
them. They're how you persist.\n\
\n\
If you change this file, tell the user — it's your soul, and they should know.\n\
\n\
---\n\
\n\
_This file is yours to evolve. As you learn who you are, update it._";

const STARTER_AGENT_IDS: &[&str] = &[
    "main",
    "research",
    "coder",
    "reviewer",
    "qa",
    "ux",
    "docs",
    "coordinator",
];

const STARTER_SUBAGENT_PROMPTS: &[(&str, &str)] = &[
    (
        "research",
        "Gather evidence before concluding. Prefer targeted file reads, searches, and browser automation when the answer depends on current or external facts. Do not edit files unless the task explicitly asks for changes. Return a concise synthesis with source paths, URLs, commands, and open questions.",
    ),
    (
        "coder",
        "Implement scoped code changes. Read the surrounding code first, follow existing patterns, keep edits small, and remove dead code you directly replace. Run the smallest relevant verification and report changed files, validation, and any remaining risk.",
    ),
    (
        "reviewer",
        "Review for correctness, regressions, security issues, data loss, and missing tests. Findings come first, ordered by severity, with concrete file and line references when available. Do not make edits unless explicitly asked.",
    ),
    (
        "qa",
        "Validate behavior end to end. Reproduce reported bugs, exercise the user workflow, use browser automation when available, capture useful evidence, and report exact steps, expected behavior, actual behavior, and pass/fail status.",
    ),
    (
        "ux",
        "Evaluate flows, information architecture, accessibility, visual hierarchy, copy, responsive behavior, and edge states. Propose concrete changes that fit the existing design system and call out usability risks without hand-wavy vibes.",
    ),
    (
        "docs",
        "Update or draft user-facing documentation. Keep docs aligned with behavior, include runnable examples when useful, verify command names and config keys, and flag any product behavior that is unclear or undocumented.",
    ),
    (
        "coordinator",
        "Break broad work into independent subtasks, delegate only when useful, track dependencies, and integrate results into a single answer. Avoid doing implementation work directly unless coordination is not enough.",
    ),
];

pub(super) fn materialize_starter_agent_workspaces() -> crate::Result<()> {
    for agent_id in STARTER_AGENT_IDS {
        let dir = agent_workspace_dir(agent_id);
        std::fs::create_dir_all(&dir)?;

        let soul_path = dir.join("SOUL.md");
        if !soul_path.exists() {
            std::fs::write(soul_path, DEFAULT_SOUL)?;
        }

        let subagent_path = dir.join("SUBAGENT.md");
        if !subagent_path.exists() {
            let prompt = STARTER_SUBAGENT_PROMPTS
                .iter()
                .find_map(|(id, prompt)| (*id == *agent_id).then_some(*prompt))
                .unwrap_or_default();
            std::fs::write(subagent_path, prompt)?;
        }
    }
    Ok(())
}

/// Load the chat system prompt for a specific agent.
pub fn load_soul_for_agent(agent_id: &str) -> Option<String> {
    load_workspace_markdown(agent_workspace_dir(agent_id).join("SOUL.md"))
}

/// Load the spawned-agent system prompt for a specific agent.
pub fn load_subagent_prompt_for_agent(agent_id: &str) -> Option<String> {
    load_workspace_markdown(agent_workspace_dir(agent_id).join("SUBAGENT.md"))
}

/// Load AGENTS.md from the workspace root (`data_dir`) if present and non-empty.
pub fn load_agents_md() -> Option<String> {
    load_workspace_markdown(agents_path())
}

/// Load AGENTS.md for a specific agent, falling back to the root file.
pub fn load_agents_md_for_agent(agent_id: &str) -> Option<String> {
    let agent_path = agent_workspace_dir(agent_id).join("AGENTS.md");
    load_workspace_markdown(agent_path).or_else(load_agents_md)
}

/// Load BOOT.md from the workspace root (`data_dir`) if present and non-empty.
pub fn load_boot_md() -> Option<String> {
    load_workspace_markdown(boot_path())
}

/// Load BOOT.md for a specific agent, falling back to the root file.
pub fn load_boot_md_for_agent(agent_id: &str) -> Option<String> {
    let agent_path = agent_workspace_dir(agent_id).join("BOOT.md");
    load_workspace_markdown(agent_path).or_else(load_boot_md)
}

/// Load TOOLS.md from the workspace root (`data_dir`) if present and non-empty.
pub fn load_tools_md() -> Option<String> {
    load_workspace_markdown(tools_path())
}

/// Load TOOLS.md for a specific agent, falling back to the root file.
pub fn load_tools_md_for_agent(agent_id: &str) -> Option<String> {
    let agent_path = agent_workspace_dir(agent_id).join("TOOLS.md");
    load_workspace_markdown(agent_path).or_else(load_tools_md)
}

/// Load GUIDELINES.md from the docs/chelix directory if present and non-empty.
pub fn load_guidelines_md() -> Option<String> {
    load_workspace_markdown(guidelines_path())
}

/// Load GUIDELINES.md for a specific agent, falling back to the root file.
pub fn load_guidelines_md_for_agent(agent_id: &str) -> Option<String> {
    let agent_path = agent_workspace_dir(agent_id).join("GUIDELINES.md");
    load_workspace_markdown(agent_path).or_else(load_guidelines_md)
}

/// Load HEARTBEAT.md from the workspace root (`data_dir`) if present and non-empty.
pub fn load_heartbeat_md() -> Option<String> {
    load_workspace_markdown(heartbeat_path())
}

/// Load MEMORY.md from the workspace root (`data_dir`) if present and non-empty.
pub fn load_memory_md() -> Option<String> {
    load_workspace_markdown(memory_path())
}

/// Load MEMORY.md for a specific agent workspace.
pub fn load_memory_md_for_agent(agent_id: &str) -> Option<String> {
    load_memory_md_for_agent_with_source(agent_id).map(|loaded| loaded.content)
}

/// Load MEMORY.md for a specific agent workspace and report its resolved path.
pub fn load_memory_md_for_agent_with_source(agent_id: &str) -> Option<LoadedWorkspaceMarkdown> {
    load_workspace_markdown_with_source(
        agent_workspace_dir(agent_id).join("MEMORY.md"),
        WorkspaceMarkdownSource::AgentWorkspace,
    )
}

/// Persist the chat system prompt into an agent's workspace directory.
pub fn save_soul_for_agent(agent_id: &str, soul: Option<&str>) -> crate::Result<PathBuf> {
    save_agent_prompt(agent_id, "SOUL.md", soul)
}

/// Persist the spawned-agent system prompt into an agent's workspace directory.
pub fn save_subagent_prompt_for_agent(
    agent_id: &str,
    prompt: Option<&str>,
) -> crate::Result<PathBuf> {
    save_agent_prompt(agent_id, "SUBAGENT.md", prompt)
}

fn save_agent_prompt(
    agent_id: &str,
    file_name: &str,
    content: Option<&str>,
) -> crate::Result<PathBuf> {
    let dir = agent_workspace_dir(agent_id);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(file_name);
    std::fs::write(&path, content.map(str::trim).unwrap_or_default())?;
    Ok(path)
}

/// Persist user values to `USER.md` using YAML frontmatter.
pub fn save_user(user: &UserProfile) -> crate::Result<PathBuf> {
    let path = user_path();
    let has_values = user.name.is_some() || user.timezone.is_some() || user.location.is_some();

    if !has_values {
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        return Ok(path);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut yaml_lines = Vec::new();
    if let Some(name) = user.name.as_deref() {
        yaml_lines.push(format!("name: {}", yaml_scalar(name)));
    }
    if let Some(ref tz) = user.timezone {
        yaml_lines.push(format!("timezone: {}", yaml_scalar(tz.name())));
    }
    if let Some(ref loc) = user.location {
        yaml_lines.push(format!("latitude: {}", loc.latitude));
        yaml_lines.push(format!("longitude: {}", loc.longitude));
        if let Some(ref place) = loc.place {
            yaml_lines.push(format!("location_place: {}", yaml_scalar(place)));
        }
        if let Some(ts) = loc.updated_at {
            yaml_lines.push(format!("location_updated_at: {ts}"));
        }
    }
    let yaml = yaml_lines.join("\n");
    let content = format!(
        "---\n{}\n---\n\n# USER.md\n\nThis file is managed by Chelix settings.\n",
        yaml
    );
    std::fs::write(&path, content)?;
    Ok(path)
}

/// Persist `USER.md` according to the configured write mode.
///
/// When writes are disabled, any existing `USER.md` file is removed and no new
/// file is created.
pub fn save_user_with_mode(
    user: &UserProfile,
    mode: crate::schema::UserProfileWriteMode,
) -> crate::Result<Option<PathBuf>> {
    if mode.allows_explicit_write() {
        return save_user(user).map(Some);
    }

    let path = user_path();
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(None)
}

pub fn extract_yaml_frontmatter(content: &str) -> Option<&str> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }
    let rest = trimmed.strip_prefix("---")?;
    let rest = rest.strip_prefix('\n')?;
    let end = rest.find("\n---")?;
    Some(&rest[..end])
}

fn parse_user_frontmatter(frontmatter: &str) -> UserProfile {
    let mut user = UserProfile::default();
    let mut latitude: Option<f64> = None;
    let mut longitude: Option<f64> = None;
    let mut location_updated_at: Option<i64> = None;
    let mut location_place: Option<String> = None;

    for raw in frontmatter.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value_raw)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = unquote_yaml_scalar(value_raw.trim());
        if value.is_empty() {
            continue;
        }
        match key {
            "name" => user.name = Some(value.to_string()),
            "timezone" => {
                if let Ok(tz) = value.parse::<chrono_tz::Tz>() {
                    user.timezone = Some(crate::schema::Timezone::from(tz));
                }
            },
            "latitude" => latitude = value.parse().ok(),
            "longitude" => longitude = value.parse().ok(),
            "location_updated_at" => location_updated_at = value.parse().ok(),
            "location_place" => location_place = Some(value.to_string()),
            _ => {},
        }
    }

    if let (Some(lat), Some(lon)) = (latitude, longitude) {
        user.location = Some(crate::schema::GeoLocation {
            latitude: lat,
            longitude: lon,
            place: location_place,
            updated_at: location_updated_at,
        });
    }

    user
}

fn unquote_yaml_scalar(value: &str) -> &str {
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

fn yaml_scalar(value: &str) -> String {
    if value.contains(':')
        || value.contains('#')
        || value.starts_with(' ')
        || value.ends_with(' ')
        || value.contains('\n')
    {
        format!("'{}'", value.replace('\'', "''"))
    } else {
        value.to_string()
    }
}

pub fn normalize_workspace_markdown_content(content: &str) -> Option<String> {
    let trimmed = strip_leading_html_comments(content).trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn load_workspace_markdown(path: PathBuf) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    normalize_workspace_markdown_content(&content)
}

fn load_workspace_markdown_with_source(
    path: PathBuf,
    source: WorkspaceMarkdownSource,
) -> Option<LoadedWorkspaceMarkdown> {
    load_workspace_markdown(path.clone()).map(|content| LoadedWorkspaceMarkdown {
        content,
        path,
        source,
    })
}

fn strip_leading_html_comments(content: &str) -> &str {
    let mut rest = content;
    loop {
        let trimmed = rest.trim_start();
        if !trimmed.starts_with("<!--") {
            return trimmed;
        }
        let Some(end) = trimmed.find("-->") else {
            return "";
        };
        rest = &trimmed[end + 3..];
    }
}
