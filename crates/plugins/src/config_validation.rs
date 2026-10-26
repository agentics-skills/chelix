//! Hook event-name checks for a parsed Chelix config.

use chelix_config::{ChelixConfig, Diagnostic, DiagnosticCategory, Severity};

const VALID_HOOK_EVENTS: &[&str] = &[
    "BeforeAgentStart",
    "BeforeLLMCall",
    "AfterLLMCall",
    "MessageReceived",
    "MessageSending",
    "MessageSent",
    "BeforeToolCall",
    "AfterToolCall",
    "ToolResultPersist",
    "SessionStart",
    "SessionEnd",
    "GatewayStart",
    "GatewayStop",
    "Command",
];

/// Warn when a configured shell hook lists an unknown event name.
#[must_use]
pub fn validate_config(config: &ChelixConfig) -> Vec<Diagnostic> {
    let Some(hooks_config) = config.hooks.as_ref() else {
        return Vec::new();
    };
    let mut diagnostics = Vec::new();
    for (hook_idx, hook) in hooks_config.hooks.iter().enumerate() {
        for (ev_idx, event) in hook.events.iter().enumerate() {
            if !VALID_HOOK_EVENTS.contains(&event.as_str()) {
                diagnostics.push(Diagnostic {
                    severity: Severity::Warning,
                    summary_message: None,
                    category: DiagnosticCategory::UnknownField,
                    path: format!("hooks.hooks[{hook_idx}].events[{ev_idx}]"),
                    message: format!(
                        "unknown hook event \"{event}\"; expected one of: {}",
                        VALID_HOOK_EVENTS.join(", ")
                    ),
                });
            }
        }
    }
    diagnostics
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use chelix_config::schema::{HooksConfig, ShellHookConfigEntry};

    use super::*;

    fn hook(events: &[&str]) -> ChelixConfig {
        ChelixConfig {
            hooks: Some(HooksConfig {
                hooks: vec![ShellHookConfigEntry {
                    name: "test".into(),
                    command: "echo test".into(),
                    events: events.iter().map(|event| (*event).to_string()).collect(),
                    timeout: 10,
                    env: HashMap::new(),
                }],
            }),
            ..Default::default()
        }
    }

    #[test]
    fn absent_hooks_are_accepted() {
        assert!(validate_config(&ChelixConfig::default()).is_empty());
    }

    #[test]
    fn unknown_hook_event_is_warned() {
        let diagnostics = validate_config(&hook(&["BeforeToolCall", "NotAnEvent"]));
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.severity == Severity::Warning
                && diagnostic.category == DiagnosticCategory::UnknownField
                && diagnostic.path == "hooks.hooks[0].events[1]"
                && diagnostic.message.contains("NotAnEvent")
        }));
        assert!(
            diagnostics
                .iter()
                .all(|diagnostic| diagnostic.path != "hooks.hooks[0].events[0]")
        );
    }

    #[test]
    fn known_hook_events_are_accepted() {
        let diagnostics = validate_config(&hook(VALID_HOOK_EVENTS));
        assert!(diagnostics.is_empty());
    }
}
