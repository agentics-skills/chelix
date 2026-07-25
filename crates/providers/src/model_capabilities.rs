//! Complete model records exposed by the provider registry.

use chelix_common::{ModelMetadata, ModelModality};

/// Info about an available model.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelInfo {
    pub id: String,
    pub provider: String,
    pub display_name: String,
    /// Unix timestamp from the provider API.
    pub created_at: Option<i64>,
    pub recommended: bool,
    /// The registry's canonical, fully resolved source of model parameters.
    #[serde(flatten)]
    pub metadata: ModelMetadata,
}

impl ModelInfo {
    #[must_use]
    pub fn supports_text_chat(&self) -> bool {
        self.metadata.supports_input(ModelModality::Text)
            && self.metadata.supports_output(ModelModality::Text)
    }
}
