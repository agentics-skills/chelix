//! Shared tool allow/deny policy.

use serde::{Deserialize, Serialize};

/// Glob-based allow/deny policy for tool access.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolPolicy {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny: Vec<String>,
}

fn pattern_matches(pattern: &str, name: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return name.starts_with(prefix);
    }
    pattern == name
}

impl ToolPolicy {
    /// Return whether a tool name is allowed by this policy.
    #[must_use]
    pub fn is_allowed(&self, tool_name: &str) -> bool {
        if self
            .deny
            .iter()
            .any(|pattern| pattern_matches(pattern, tool_name))
        {
            return false;
        }
        self.allow.is_empty()
            || self
                .allow
                .iter()
                .any(|pattern| pattern_matches(pattern, tool_name))
    }

    /// Merge a higher-precedence policy into this policy.
    #[must_use]
    pub fn merge_with(&self, other: &Self) -> Self {
        Self {
            allow: if other.allow.is_empty() {
                self.allow.clone()
            } else {
                other.allow.clone()
            },
            deny: {
                let mut combined = self.deny.clone();
                combined.extend(other.deny.iter().cloned());
                combined
            },
        }
    }
}
