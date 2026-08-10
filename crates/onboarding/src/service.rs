//! Live onboarding service that backs the `wizard.*` and `user.*` RPC methods.

use std::{path::PathBuf, sync::Mutex};

use {
    serde::Deserialize,
    serde_json::{Value, json},
};

use chelix_config::{AgentConfig, ChelixConfig, GeoLocation, Timezone, UserProfile};

use crate::{
    Context, Error, Result,
    state::{WizardState, WizardStep},
};

/// Live onboarding service backed by a `WizardState` and config persistence.
pub struct LiveOnboardingService {
    state: Mutex<Option<WizardState>>,
    config_path: PathBuf,
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

impl LiveOnboardingService {
    pub fn new(config_path: PathBuf) -> Self {
        Self {
            state: Mutex::new(None),
            config_path,
        }
    }

    /// Save config to the service's config path.
    fn save(&self, config: &ChelixConfig) -> Result<()> {
        chelix_config::loader::save_config_to_path(&self.config_path, config)
            .context("failed to save onboarding config")?;
        Ok(())
    }

    fn load_existing(&self) -> Result<ChelixConfig> {
        if !self.config_path.exists() {
            return Err(Error::message(format!(
                "onboarding config does not exist: {}",
                self.config_path.display()
            )));
        }
        chelix_config::loader::load_config(&self.config_path)
            .context("failed to load existing onboarding config")
    }

    /// Check whether onboarding has been completed.
    ///
    /// Returns `true` when the `.onboarded` sentinel file exists in the data
    /// directory or the `SKIP_ONBOARDING` environment variable is non-empty.
    fn is_already_onboarded(&self) -> bool {
        if std::env::var("SKIP_ONBOARDING")
            .ok()
            .is_some_and(|value| !value.is_empty())
        {
            return true;
        }
        onboarded_sentinel().exists()
    }

    /// Mark onboarding as complete by writing the sentinel file.
    fn mark_onboarded(&self) -> Result<()> {
        let path = onboarded_sentinel();
        std::fs::write(&path, "").context("failed to mark onboarding complete")?;
        Ok(())
    }

    /// Start the wizard and return current step information.
    pub fn wizard_start(&self, force: bool) -> Result<Value> {
        let config = self.load_existing()?;
        let (_, default_agent) = configured_default_agent(&config)?;

        if !force && self.is_already_onboarded() {
            return Ok(json!({
                "onboarded": true,
                "step": "done",
                "prompt": "Already onboarded!",
            }));
        }

        let mut state = WizardState::new();
        state.agent = default_agent.clone();
        state.user = chelix_config::resolve_user_profile_from_config(&config);

        let response = step_response(&state);
        *self.state.lock().unwrap_or_else(|error| error.into_inner()) = Some(state);
        Ok(response)
    }

    /// Advance the wizard with user input.
    pub fn wizard_next(&self, input: &str) -> Result<Value> {
        let mut guard = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let state = guard.as_mut().context("no active wizard session")?;
        state.advance(input);

        if state.is_done() {
            let mut config = self.load_existing()?;
            let (default_id, _) = configured_default_agent(&config)?;
            let default_id = default_id.to_string();

            config
                .agents
                .entries
                .insert(default_id.clone(), state.agent.clone());
            config.user = state.user.clone();

            self.save(&config)?;
            chelix_config::save_user_with_mode(&config.user, config.memory.user_profile_write_mode)
                .context("failed to save user profile")?;
            self.mark_onboarded()?;

            let response = json!({
                "step": "done",
                "prompt": state.prompt(),
                "done": true,
                "agent_id": default_id,
                "agent": state.agent.clone(),
                "user": config.user.clone(),
            });
            *guard = None;
            return Ok(response);
        }

        Ok(step_response(state))
    }

    /// Cancel an active wizard session.
    pub fn wizard_cancel(&self) {
        *self.state.lock().unwrap_or_else(|error| error.into_inner()) = None;
    }

    /// Return the current wizard status.
    pub fn wizard_status(&self) -> Value {
        let guard = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let onboarded = self.is_already_onboarded();
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
    pub fn user_get(&self) -> Result<Value> {
        let config = self.load_existing()?;
        configured_default_agent(&config)?;
        serde_json::to_value(config.user).context("failed to serialize user profile")
    }

    /// Replace the canonical user profile stored under `[user]`.
    pub fn user_update(&self, params: Value) -> Result<Value> {
        let update: UserUpdateParams =
            serde_json::from_value(params).context("invalid user update payload")?;
        let user = update.into_profile();
        let mut config = self.load_existing()?;
        let (_, default_agent) = configured_default_agent(&config)?;
        let agent_name_is_set = !default_agent.name.trim().is_empty();

        config.user = user.clone();
        self.save(&config)?;
        chelix_config::save_user_with_mode(&user, config.memory.user_profile_write_mode)
            .context("failed to save user profile")?;

        if agent_name_is_set && user.name.is_some() {
            self.mark_onboarded()?;
        }

        serde_json::to_value(user).context("failed to serialize user profile")
    }
}

/// Return the configured default agent without normalization or fallback.
fn configured_default_agent(config: &ChelixConfig) -> Result<(&str, &AgentConfig)> {
    let default_id = config.agents.default.as_str();
    if default_id.trim().is_empty() {
        return Err(Error::message("agents.default is empty"));
    }
    let agent = config.agents.entries.get(default_id).ok_or_else(|| {
        Error::message(format!(
            "default agent \"{default_id}\" is not defined under [agents]"
        ))
    })?;
    Ok((default_id, agent))
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
    use super::*;

    struct TestDataDirState {
        _data_dir: Option<PathBuf>,
    }

    static DATA_DIR_TEST_LOCK: Mutex<TestDataDirState> =
        Mutex::new(TestDataDirState { _data_dir: None });

    fn write_config(path: &std::path::Path, default_id: &str) {
        let mut config = ChelixConfig::default();
        config.agents.default = default_id.to_string();
        config
            .agents
            .entries
            .insert(default_id.to_string(), AgentConfig {
                name: "chelix".to_string(),
                ..AgentConfig::default()
            });
        chelix_config::loader::save_config_to_path(path, &config).unwrap();
    }

    #[test]
    fn wizard_round_trip_updates_default_agent() {
        let _guard = DATA_DIR_TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        chelix_config::set_data_dir(dir.path().to_path_buf());
        let config_path = dir.path().join("chelix.toml");
        write_config(&config_path, "assistant");
        let service = LiveOnboardingService::new(config_path.clone());

        let response = service.wizard_start(false).expect("start wizard");
        assert_eq!(response["onboarded"], false);
        assert_eq!(response["step"], "welcome");

        service.wizard_next("").unwrap();
        service.wizard_next("Alice").unwrap();
        service.wizard_next("Rex").unwrap();
        service.wizard_next("\u{1f436}").unwrap();
        let done = service.wizard_next("").unwrap();

        assert_eq!(done["done"], true);
        assert_eq!(done["agent_id"], "assistant");
        assert_eq!(done["agent"]["name"], "Rex");
        assert_eq!(done["user"]["name"], "Alice");

        let saved = chelix_config::loader::load_config(&config_path).unwrap();
        assert_eq!(saved.agents.default, "assistant");
        assert_eq!(saved.agents.entries["assistant"].name, "Rex");
        assert_eq!(saved.user.name.as_deref(), Some("Alice"));
        assert!(!dir.path().join("agents/assistant/IDENTITY.md").exists());
        assert!(dir.path().join("USER.md").exists());
        chelix_config::clear_data_dir();
    }

    #[test]
    fn sentinel_file_marks_onboarded() {
        let _guard = DATA_DIR_TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        chelix_config::set_data_dir(dir.path().to_path_buf());
        let config_path = dir.path().join("chelix.toml");
        write_config(&config_path, "assistant");
        std::fs::write(dir.path().join(".onboarded"), "").unwrap();

        let service = LiveOnboardingService::new(config_path);
        let response = service.wizard_start(false).expect("start wizard");
        assert_eq!(response["onboarded"], true);
        chelix_config::clear_data_dir();
    }

    #[test]
    fn cancel_wizard() {
        let _guard = DATA_DIR_TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        chelix_config::set_data_dir(dir.path().to_path_buf());
        let config_path = dir.path().join("chelix.toml");
        write_config(&config_path, "assistant");
        let service = LiveOnboardingService::new(config_path);

        service.wizard_start(false).expect("start wizard");
        assert_eq!(service.wizard_status()["active"], true);
        service.wizard_cancel();
        assert_eq!(service.wizard_status()["active"], false);
        chelix_config::clear_data_dir();
    }

    #[test]
    fn user_update_replaces_profile() {
        let _guard = DATA_DIR_TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        chelix_config::set_data_dir(dir.path().to_path_buf());
        let config_path = dir.path().join("chelix.toml");
        write_config(&config_path, "assistant");
        let service = LiveOnboardingService::new(config_path.clone());

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
            .unwrap();

        assert_eq!(response["name"], "Alice");
        assert_eq!(response["timezone"], "America/New_York");
        assert_eq!(response["location"]["place"], "San Francisco");
        assert!(response["location"]["updated_at"].is_number());

        let saved = chelix_config::loader::load_config(&config_path).unwrap();
        assert_eq!(saved.user.name.as_deref(), Some("Alice"));
        assert_eq!(
            saved.user.timezone.as_ref().map(Timezone::name),
            Some("America/New_York")
        );
        chelix_config::clear_data_dir();
    }

    #[test]
    fn user_update_rejects_unknown_fields() {
        let _guard = DATA_DIR_TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        chelix_config::set_data_dir(dir.path().to_path_buf());
        let config_path = dir.path().join("chelix.toml");
        write_config(&config_path, "assistant");
        let service = LiveOnboardingService::new(config_path);

        service
            .user_update(json!({ "user_name": "Alice" }))
            .expect_err("legacy field must be rejected");
        chelix_config::clear_data_dir();
    }

    #[test]
    fn unknown_default_agent_is_rejected_without_writing() {
        let _guard = DATA_DIR_TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        chelix_config::set_data_dir(dir.path().to_path_buf());
        let config_path = dir.path().join("chelix.toml");
        write_config(&config_path, "assistant");
        let mut config = chelix_config::loader::load_config(&config_path).unwrap();
        config.agents.default = "missing".to_string();
        chelix_config::loader::save_config_to_path(&config_path, &config).unwrap();
        let before = std::fs::read_to_string(&config_path).unwrap();
        let service = LiveOnboardingService::new(config_path.clone());

        service
            .wizard_start(false)
            .expect_err("unknown default must fail");
        service
            .user_update(json!({ "name": "Alice" }))
            .expect_err("unknown default must fail");
        assert_eq!(std::fs::read_to_string(config_path).unwrap(), before);
        chelix_config::clear_data_dir();
    }

    #[test]
    fn invalid_existing_config_is_never_overwritten() {
        let _guard = DATA_DIR_TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        chelix_config::set_data_dir(dir.path().to_path_buf());
        let config_path = dir.path().join("chelix.toml");
        let invalid = "[server]\nremoved_option = true\n";
        std::fs::write(&config_path, invalid).unwrap();
        let service = LiveOnboardingService::new(config_path.clone());

        service
            .wizard_start(false)
            .expect_err("invalid config must fail");
        service
            .user_update(json!({ "name": "Alice" }))
            .expect_err("user update must reject invalid config");

        assert_eq!(std::fs::read_to_string(config_path).unwrap(), invalid);
        chelix_config::clear_data_dir();
    }
}
