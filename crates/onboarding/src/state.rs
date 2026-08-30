//! Pure state machine for the onboarding wizard. No I/O.

use chelix_config::{AgentConfig, UserProfile};

/// Steps in the onboarding wizard.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WizardStep {
    #[default]
    Welcome,
    UserName,
    AgentName,
    AgentEmoji,
    Confirm,
    Done,
}

/// Editable presentation fields for an already configured agent.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct AgentIdentityDraft {
    pub name: String,
    pub emoji: Option<String>,
}

impl AgentIdentityDraft {
    #[must_use]
    pub fn from_agent(agent: &AgentConfig) -> Self {
        Self {
            name: agent.name.clone(),
            emoji: agent.emoji.clone(),
        }
    }

    pub fn apply_to(&self, agent: &mut AgentConfig) {
        agent.name.clone_from(&self.name);
        agent.emoji.clone_from(&self.emoji);
    }
}

/// The wizard state, advanced one step at a time.
#[derive(Debug, Clone, Default)]
pub struct WizardState {
    pub step: WizardStep,
    pub user: UserProfile,
    pub agent: AgentIdentityDraft,
}

impl WizardState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The prompt text to display for the current step.
    pub fn prompt(&self) -> &str {
        match self.step {
            WizardStep::Welcome => {
                "Welcome to chelix! Let's set things up. Press Enter to continue."
            },
            WizardStep::UserName => "What's your name?",
            WizardStep::AgentName => "Pick a name for your agent:",
            WizardStep::AgentEmoji => "Choose an emoji for your agent (e.g. \u{1f916}):",
            WizardStep::Confirm => "All set! Press Enter to save, or type 'back' to go back.",
            WizardStep::Done => "Onboarding complete!",
        }
    }

    /// Process user input and advance to the next step.
    pub fn advance(&mut self, input: &str) {
        let input = input.trim();
        match self.step {
            WizardStep::Welcome => self.step = WizardStep::UserName,
            WizardStep::UserName => {
                if !input.is_empty() {
                    self.user.name = Some(input.to_string());
                }
                self.step = WizardStep::AgentName;
            },
            WizardStep::AgentName => {
                if !input.is_empty() {
                    self.agent.name = input.to_string();
                } else if self.agent.name.trim().is_empty() {
                    self.agent.name = "chelix".to_string();
                }
                self.step = WizardStep::AgentEmoji;
            },
            WizardStep::AgentEmoji => {
                if !input.is_empty() {
                    self.agent.emoji = Some(input.to_string());
                }
                self.step = WizardStep::Confirm;
            },
            WizardStep::Confirm => {
                if input.eq_ignore_ascii_case("back") {
                    self.step = WizardStep::AgentEmoji;
                } else {
                    self.step = WizardStep::Done;
                }
            },
            WizardStep::Done => {},
        }
    }

    #[must_use]
    pub fn is_done(&self) -> bool {
        self.step == WizardStep::Done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_wizard_flow() {
        let mut state = WizardState::new();
        assert_eq!(state.step, WizardStep::Welcome);

        state.advance("");
        assert_eq!(state.step, WizardStep::UserName);

        state.advance("Alice");
        assert_eq!(state.user.name.as_deref(), Some("Alice"));
        assert_eq!(state.step, WizardStep::AgentName);

        state.advance("Momo");
        assert_eq!(state.agent.name, "Momo");

        state.advance("\u{1f99c}");
        assert_eq!(state.agent.emoji.as_deref(), Some("\u{1f99c}"));
        assert_eq!(state.step, WizardStep::Confirm);

        state.advance("");
        assert!(state.is_done());
    }

    #[test]
    fn back_from_confirm() {
        let mut state = WizardState::new();
        state.advance("");
        state.advance("Bob");
        state.advance("Rex");
        state.advance("\u{1f436}");
        assert_eq!(state.step, WizardStep::Confirm);

        state.advance("back");
        assert_eq!(state.step, WizardStep::AgentEmoji);
    }

    #[test]
    fn default_agent_name() {
        let mut state = WizardState::new();
        state.advance("");
        state.advance("User");
        state.advance("");
        assert_eq!(state.agent.name, "chelix");
    }
}
