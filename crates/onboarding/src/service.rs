//! Live onboarding service that backs the `wizard.*` and `user.*` RPC methods.

use std::path::PathBuf;

use {
    serde::Deserialize,
    serde_json::{Value, json},
    tokio::sync::Mutex,
};

use {
    chelix_config::{
        AgentConfig, ChelixConfig, GeoLocation, Timezone, UserProfile, UserProfileWriteMode,
    },
    chelix_config_service::ConfigService,
};

use crate::{
    Context, Error, Result,
    state::{AgentIdentityDraft, WizardState, WizardStep},
};

/// Live onboarding service backed by wizard state and the configuration service.
pub struct LiveOnboardingService {
    state: Mutex<Option<WizardState>>,
    config: ConfigService,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UserUpdateParams {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    timezone: Option<Timezone>,
    #[serde(default)]
    location: Option<UserLocationParams>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UserLocationParams {
    latitude: f64,
    longitude: f64,
    #[serde(default)]
    place: Option<String>,
}

impl UserUpdateParams {
    fn into_profile(self) -> UserProfile {
        UserProfile {
            name: self
                .name
                .map(|name| name.trim().to_string())
                .filter(|name| !name.is_empty()),
            timezone: self.timezone,
            location: self.location.map(|location| {
                GeoLocation::now(
                    location.latitude,
                    location.longitude,
                    location
                        .place
                        .map(|place| place.trim().to_string())
                        .filter(|place| !place.is_empty()),
                )
            }),
        }
    }
}

struct WizardSeed {
    agent: AgentIdentityDraft,
    user: UserProfile,
}

pub(crate) struct IdentityPatch {
    pub(crate) default_id: String,
    pub(crate) agent: AgentConfig,
    pub(crate) user: UserProfile,
    pub(crate) write_mode: UserProfileWriteMode,
}

struct UserPatch {
    user: UserProfile,
    write_mode: UserProfileWriteMode,
    agent_name_is_set: bool,
}

impl LiveOnboardingService {
    pub fn new(config: ConfigService) -> Self {
        Self {
            state: Mutex::new(None),
            config,
        }
    }

    /// Start the wizard and return current step information.
    pub async fn wizard_start(&self, force: bool) -> Result<Value> {
        let config = self.config.clone();
        let mut guard = self.state.lock().await;
        let seed = read_wizard_seed(&config).await?;

        if !force && is_already_onboarded() {
            return Ok(json!({
                "onboarded": true,
                "step": "done",
                "prompt": "Already onboarded!",
            }));
        }

        let mut state = WizardState::new();
        state.agent = seed.agent;
        state.user = seed.user;

        let response = step_response(&state);
        *guard = Some(state);
        Ok(response)
    }

    /// Advance the wizard with user input.
    pub async fn wizard_next(&self, input: &str) -> Result<Value> {
        let config = self.config.clone();
        let mut guard = self.state.lock().await;
        let (prompt, user, agent) = {
            let state = guard.as_mut().context("no active wizard session")?;
            state.advance(input);
            if !state.is_done() {
                return Ok(step_response(state));
            }
            (
                state.prompt().to_string(),
                state.user.clone(),
                state.agent.clone(),
            )
        };

        let patched = config
            .update(move |config| patch_wizard_identity(config, user, agent))
            .await?;
        chelix_config::save_user_with_mode(&patched.user, patched.write_mode)
            .context("failed to save user profile")?;
        mark_onboarded()?;

        let response = json!({
            "step": "done",
            "prompt": prompt,
            "done": true,
            "agent_id": patched.default_id,
            "agent": patched.agent,
            "user": patched.user,
        });
        *guard = None;
        Ok(response)
    }

    /// Cancel an active wizard session.
    pub async fn wizard_cancel(&self) {
        *self.state.lock().await = None;
    }

    /// Return the current wizard status.
    pub async fn wizard_status(&self) -> Value {
        let guard = self.state.lock().await;
        let onboarded = is_already_onboarded();
        match guard.as_ref() {
            Some(state) => json!({
                "active": true,
                "step": state.step,
                "onboarded": onboarded,
            }),
            None => json!({
                "active": false,
                "onboarded": onboarded,
            }),
        }
    }

    /// Return the canonical user profile stored under `[user]`.
    pub async fn user_get(&self) -> Result<Value> {
        self.config
            .read(|config| serde_json::to_value(&config.user))
            .await?
            .context("failed to serialize user profile")
    }

    /// Replace the canonical user profile stored under `[user]`.
    pub async fn user_update(&self, params: Value) -> Result<Value> {
        let update: UserUpdateParams =
            serde_json::from_value(params).context("invalid user update payload")?;
        let user = update.into_profile();
        let patched = self
            .config
            .update(move |config| patch_user(config, user))
            .await?;
        chelix_config::save_user_with_mode(&patched.user, patched.write_mode)
            .context("failed to save user profile")?;

        if patched.agent_name_is_set && patched.user.name.is_some() {
            mark_onboarded()?;
        }

        serde_json::to_value(patched.user).context("failed to serialize user profile")
    }
}

/// Returns `true` when the `.onboarded` sentinel file exists in the data
/// directory or the `SKIP_ONBOARDING` environment variable is non-empty.
fn is_already_onboarded() -> bool {
    if std::env::var("SKIP_ONBOARDING")
        .ok()
        .is_some_and(|value| !value.is_empty())
    {
        return true;
    }
    onboarded_sentinel().exists()
}

/// Mark onboarding as complete by writing the sentinel file.
fn mark_onboarded() -> Result<()> {
    let path = onboarded_sentinel();
    std::fs::write(&path, "").context("failed to mark onboarding complete")?;
    Ok(())
}

async fn read_wizard_seed(config: &ConfigService) -> Result<WizardSeed> {
    let loaded = config.read(Clone::clone).await?;
    let (_, agent) = configured_default_agent(&loaded)?;
    Ok(WizardSeed {
        agent: AgentIdentityDraft::from_agent(agent),
        user: chelix_config::resolve_user_profile_from_config(&loaded),
    })
}

/// Return the configured default agent without normalization or fallback.
pub(crate) fn configured_default_agent(config: &ChelixConfig) -> Result<(&str, &AgentConfig)> {
    match config
        .agents
        .resolve_state()
        .map_err(|error| Error::message(error.to_string()))?
    {
        chelix_config::AgentsConfigState::Setup => Err(Error::message(
            "default agent is not configured; configure a provider and model, then complete web onboarding",
        )),
        chelix_config::AgentsConfigState::Configured {
            default_id,
            default_agent,
        } => Ok((default_id, default_agent)),
    }
}

pub(crate) fn patch_wizard_identity(
    config: &mut ChelixConfig,
    user: UserProfile,
    agent: AgentIdentityDraft,
) -> chelix_config::Result<IdentityPatch> {
    let default_id = match configured_default_agent(config) {
        Ok((default_id, _)) => default_id.to_string(),
        Err(error) => return Err(chelix_config::Error::message(error.to_string())),
    };
    let Some(default_agent) = config.agents.entries.get_mut(&default_id) else {
        return Err(chelix_config::Error::message(
            "configured default agent disappeared",
        ));
    };
    agent.apply_to(default_agent);
    config.user = user;
    let saved_agent = config
        .agents
        .entries
        .get(&default_id)
        .cloned()
        .ok_or_else(|| chelix_config::Error::message("configured default agent disappeared"))?;
    Ok(IdentityPatch {
        default_id,
        agent: saved_agent,
        user: config.user.clone(),
        write_mode: config.memory.user_profile_write_mode,
    })
}

fn patch_user(config: &mut ChelixConfig, user: UserProfile) -> chelix_config::Result<UserPatch> {
    let agent_name_is_set = match config
        .agents
        .resolve_state()
        .map_err(|error| chelix_config::Error::message(error.to_string()))?
    {
        chelix_config::AgentsConfigState::Setup => false,
        chelix_config::AgentsConfigState::Configured { default_agent, .. } => {
            !default_agent.name.trim().is_empty()
        },
    };
    config.user = user;
    Ok(UserPatch {
        user: config.user.clone(),
        write_mode: config.memory.user_profile_write_mode,
        agent_name_is_set,
    })
}

/// Path to the `.onboarded` sentinel file in the data directory.
fn onboarded_sentinel() -> PathBuf {
    chelix_config::data_dir().join(".onboarded")
}

fn step_response(state: &WizardState) -> Value {
    json!({
        "step": state.step,
        "prompt": state.prompt(),
        "done": state.step == WizardStep::Done,
        "onboarded": false,
        "current": current_value(state),
    })
}

/// Return the current pre-populated value for the active step.
fn current_value(state: &WizardState) -> Option<&str> {
    use WizardStep::{AgentEmoji, AgentName, UserName};
    match state.step {
        UserName => state.user.name.as_deref(),
        AgentName => Some(state.agent.name.as_str()),
        AgentEmoji => state.agent.emoji.as_deref(),
        _ => None,
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use std::{
        path::Path,
        sync::{Arc, OnceLock},
    };

    use {chelix_config_service::ConfigValidation, tokio::sync::OwnedMutexGuard};

    use super::*;

    fn dir_lock() -> &'static Arc<Mutex<Option<tempfile::TempDir>>> {
        static LOCK: OnceLock<Arc<Mutex<Option<tempfile::TempDir>>>> = OnceLock::new();
        LOCK.get_or_init(|| Arc::new(Mutex::new(None)))
    }

    struct DirGuard {
        guard: OwnedMutexGuard<Option<tempfile::TempDir>>,
    }

    impl DirGuard {
        async fn enter(dir: tempfile::TempDir) -> Self {
            let mut guard = dir_lock().clone().lock_owned().await;
            *guard = Some(dir);
            let state = Self { guard };
            chelix_config::set_config_dir(state.path().to_path_buf());
            chelix_config::set_data_dir(state.path().to_path_buf());
            state
        }

        fn path(&self) -> &Path {
            self.guard
                .as_ref()
                .expect("fixture directory is owned by guard")
                .path()
        }
    }

    impl Drop for DirGuard {
        fn drop(&mut self) {
            chelix_config::clear_config_dir();
            chelix_config::clear_data_dir();
            *self.guard = None;
        }
    }

    const REQUIRED_TABLES: &str = "\
[tools.execute_command]
terminal_size = \"115x58\"

[sandbox]
archived_session_retention_days = 3
";

    fn write_config(dir: &Path, contents: &str) {
        std::fs::write(dir.join("chelix.toml"), contents).unwrap();
    }

    fn configured_agent(default_id: &str) -> String {
        format!(
            "\
[agents]
default = \"{default_id}\"

[agents.{default_id}]
name = \"chelix\"
model = \"test::model\"
reasoning_effort = \"off\"
max_tools_threshold = 128
compaction_reminder = true
prepend_sender_badge = true

{REQUIRED_TABLES}\
"
        )
    }

    async fn open_service() -> ConfigService {
        ConfigService::open(
            ConfigValidation::default(),
            chelix_config_service::ConfigInitialization::default(),
        )
        .await
        .expect("open config service")
    }

    #[tokio::test]
    async fn wizard_round_trip_updates_default_agent() {
        let dir = tempfile::tempdir().unwrap();
        let guard = DirGuard::enter(dir).await;
        write_config(guard.path(), &configured_agent("assistant"));
        let config = open_service().await;
        let service = LiveOnboardingService::new(config.clone());

        let response = service.wizard_start(false).await.expect("start wizard");
        assert_eq!(response["onboarded"], false);
        assert_eq!(response["step"], "welcome");

        service.wizard_next("").await.unwrap();
        service.wizard_next("Alice").await.unwrap();
        service.wizard_next("Rex").await.unwrap();
        service.wizard_next("\u{1f436}").await.unwrap();
        let done = service.wizard_next("").await.unwrap();

        assert_eq!(done["done"], true);
        assert_eq!(done["agent_id"], "assistant");
        assert_eq!(done["agent"]["name"], "Rex");
        assert_eq!(done["user"]["name"], "Alice");

        let saved = config
            .read(|config| match config.agents.resolve_state() {
                Ok(chelix_config::AgentsConfigState::Configured {
                    default_id,
                    default_agent,
                }) => (
                    default_id.to_string(),
                    default_agent.name.clone(),
                    config.user.name.clone(),
                ),
                Ok(chelix_config::AgentsConfigState::Setup) => (String::new(), String::new(), None),
                Err(error) => (error.to_string(), String::new(), None),
            })
            .await
            .unwrap();
        assert_eq!(saved.0, "assistant");
        assert_eq!(saved.1, "Rex");
        assert_eq!(saved.2.as_deref(), Some("Alice"));
        assert!(!guard.path().join("agents/assistant/IDENTITY.md").exists());
        assert!(guard.path().join("USER.md").exists());
    }

    #[tokio::test]
    async fn sentinel_file_marks_onboarded() {
        let dir = tempfile::tempdir().unwrap();
        let guard = DirGuard::enter(dir).await;
        write_config(guard.path(), &configured_agent("assistant"));
        std::fs::write(guard.path().join(".onboarded"), "").unwrap();

        let service = LiveOnboardingService::new(open_service().await);
        let response = service.wizard_start(false).await.expect("start wizard");
        assert_eq!(response["onboarded"], true);
    }

    #[tokio::test]
    async fn cancel_wizard() {
        let dir = tempfile::tempdir().unwrap();
        let guard = DirGuard::enter(dir).await;
        write_config(guard.path(), &configured_agent("assistant"));
        let service = LiveOnboardingService::new(open_service().await);

        service.wizard_start(false).await.expect("start wizard");
        assert_eq!(service.wizard_status().await["active"], true);
        service.wizard_cancel().await;
        assert_eq!(service.wizard_status().await["active"], false);
    }

    #[tokio::test]
    async fn user_update_replaces_profile() {
        let dir = tempfile::tempdir().unwrap();
        let guard = DirGuard::enter(dir).await;
        write_config(guard.path(), &configured_agent("assistant"));
        let config = open_service().await;
        let service = LiveOnboardingService::new(config.clone());

        let response = service
            .user_update(json!({
                "name": "Alice",
                "timezone": "America/New_York",
                "location": {
                    "latitude": 37.7749,
                    "longitude": -122.4194,
                    "place": "San Francisco"
                }
            }))
            .await
            .unwrap();

        assert_eq!(response["name"], "Alice");
        assert_eq!(response["timezone"], "America/New_York");
        assert_eq!(response["location"]["place"], "San Francisco");
        assert!(response["location"]["updated_at"].is_number());

        let saved = config
            .read(|config| {
                (
                    config.user.name.clone(),
                    config
                        .user
                        .timezone
                        .as_ref()
                        .map(Timezone::name)
                        .map(str::to_string),
                )
            })
            .await
            .unwrap();
        assert_eq!(saved.0.as_deref(), Some("Alice"));
        assert_eq!(saved.1.as_deref(), Some("America/New_York"));
    }

    #[tokio::test]
    async fn user_profile_round_trip_is_valid_during_setup() {
        let dir = tempfile::tempdir().unwrap();
        let guard = DirGuard::enter(dir).await;
        write_config(guard.path(), REQUIRED_TABLES);
        let config = open_service().await;
        let service = LiveOnboardingService::new(config.clone());

        let initial = service.user_get().await.unwrap();
        assert!(initial["name"].is_null());

        let updated = service
            .user_update(json!({
                "name": "Alice",
                "timezone": "America/New_York"
            }))
            .await
            .unwrap();
        assert_eq!(updated["name"], "Alice");

        let setup = config
            .read(|config| {
                matches!(
                    config.agents.resolve_state(),
                    Ok(chelix_config::AgentsConfigState::Setup)
                )
            })
            .await
            .unwrap();
        assert!(setup);
        assert!(!guard.path().join(".onboarded").exists());
    }

    #[tokio::test]
    async fn user_update_rejects_unknown_fields() {
        let dir = tempfile::tempdir().unwrap();
        let guard = DirGuard::enter(dir).await;
        write_config(guard.path(), &configured_agent("assistant"));
        let service = LiveOnboardingService::new(open_service().await);

        service
            .user_update(json!({ "user_name": "Alice" }))
            .await
            .expect_err("legacy field must be rejected");
    }

    #[tokio::test]
    async fn unknown_default_agent_is_rejected_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let guard = DirGuard::enter(dir).await;
        let contents = configured_agent("assistant")
            .replace("default = \"assistant\"", "default = \"missing\"");
        write_config(guard.path(), &contents);
        let config_path = guard.path().join("chelix.toml");
        let before = std::fs::read_to_string(&config_path).unwrap();

        let service = LiveOnboardingService::new(
            ConfigService::open(
                ConfigValidation::default(),
                chelix_config_service::ConfigInitialization::default(),
            )
            .await
            .expect("structural config opens"),
        );
        let error = service
            .wizard_start(false)
            .await
            .expect_err("unknown default must fail");
        assert!(
            error
                .to_string()
                .contains("default agent \"missing\" is not defined under [agents]"),
            "{error}"
        );
        assert_eq!(std::fs::read_to_string(config_path).unwrap(), before);
    }

    #[tokio::test]
    async fn invalid_existing_config_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let guard = DirGuard::enter(dir).await;
        let config_path = guard.path().join("chelix.toml");
        let invalid = "[server]\nremoved_option = true\n";
        std::fs::write(&config_path, invalid).unwrap();

        let error = ConfigService::open(
            ConfigValidation::default(),
            chelix_config_service::ConfigInitialization::default(),
        )
        .await
        .expect_err("invalid config must fail");
        assert!(error.is_validation());
        assert_eq!(std::fs::read_to_string(config_path).unwrap(), invalid);
    }
}
