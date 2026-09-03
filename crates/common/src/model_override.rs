//! Complete model/reasoning override shared by request and configuration boundaries.

use serde::{Deserialize, Serialize};

use crate::ReasoningEffort;

/// One indivisible model and reasoning effort override for camelCase transports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelOverride {
    pub model: String,
    pub reasoning_effort: ReasoningEffort,
}

/// One indivisible model and reasoning effort override for configuration surfaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigModelOverride {
    pub model: String,
    pub reasoning_effort: ReasoningEffort,
}

impl From<ConfigModelOverride> for ModelOverride {
    fn from(value: ConfigModelOverride) -> Self {
        Self {
            model: value.model,
            reasoning_effort: value.reasoning_effort,
        }
    }
}

impl From<&ConfigModelOverride> for ModelOverride {
    fn from(value: &ConfigModelOverride) -> Self {
        Self {
            model: value.model.clone(),
            reasoning_effort: value.reasoning_effort.clone(),
        }
    }
}

impl From<ModelOverride> for ConfigModelOverride {
    fn from(value: ModelOverride) -> Self {
        Self {
            model: value.model,
            reasoning_effort: value.reasoning_effort,
        }
    }
}

impl From<&ModelOverride> for ConfigModelOverride {
    fn from(value: &ModelOverride) -> Self {
        Self {
            model: value.model.clone(),
            reasoning_effort: value.reasoning_effort.clone(),
        }
    }
}
