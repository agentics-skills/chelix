//! Public plugin and provider records shared by the host and guest.

use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginStatus {
    Enabled,
    Stopping,
    Disabled,
    Failed { reason: String },
}

impl PluginStatus {
    #[must_use]
    pub fn as_label(&self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::Stopping => "stopping",
            Self::Disabled => "disabled",
            Self::Failed { .. } => "failed",
        }
    }

    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Self::Failed { reason } => reason.clone(),
            _ => String::new(),
        }
    }

    #[must_use]
    pub fn is_enabled(&self) -> bool {
        matches!(self, Self::Enabled)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInfo {
    pub name: String,
    pub api: Option<String>,
    pub version: Option<Version>,
    pub status: PluginStatus,
    pub document: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub plugin: String,
    pub order: i32,
    pub requires: Vec<String>,
    pub meta: Value,
}
