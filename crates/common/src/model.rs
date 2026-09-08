//! Canonical model metadata shared by configuration and runtime.

use {
    indexmap::IndexMap,
    serde::{Deserialize, Serialize},
    std::collections::HashSet,
};

/// Ordered provider model allowlist with per-model metadata overrides.
pub type ModelConfigMap = IndexMap<String, PartialModelMetadata>;

/// Provider-defined reasoning/thinking effort level supported by a model.
///
/// Values come from configured model metadata and are intentionally not
/// restricted to a hard-coded vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReasoningEffort(String);

impl ReasoningEffort {
    /// Exact provider wire value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ReasoningEffort {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl From<String> for ReasoningEffort {
    fn from(value: String) -> Self {
        Self(value)
    }
}

/// Canonical model ID paired with its required reasoning effort after resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModelReasoning {
    model_id: String,
    reasoning_effort: ReasoningEffort,
}

impl ResolvedModelReasoning {
    /// Build a structurally complete pair after registry resolution.
    pub fn try_new(
        model_id: String,
        reasoning_effort: ReasoningEffort,
    ) -> Result<Self, ResolvedModelReasoningError> {
        if model_id.is_empty() {
            return Err(ResolvedModelReasoningError::EmptyModelId);
        }
        if reasoning_effort.as_str().is_empty() {
            return Err(ResolvedModelReasoningError::EmptyReasoningEffort);
        }
        Ok(Self {
            model_id,
            reasoning_effort,
        })
    }

    /// Exact canonical key used by the provider registry.
    #[must_use]
    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    /// Exact reasoning effort validated against the selected model metadata.
    #[must_use]
    pub const fn reasoning_effort(&self) -> &ReasoningEffort {
        &self.reasoning_effort
    }
}

/// Structural errors rejected before a resolved pair can exist.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolvedModelReasoningError {
    #[error("resolved model ID must not be empty")]
    EmptyModelId,
    #[error("resolved reasoning effort must not be empty")]
    EmptyReasoningEffort,
}

/// Input or output medium accepted by a model endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelModality {
    Text,
    Image,
    Audio,
    Video,
    File,
}

/// OpenAI Responses reasoning summary detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningSummary {
    Auto,
    Concise,
    Detailed,
}

impl ReasoningSummary {
    /// Exact OpenAI Responses wire value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Concise => "concise",
            Self::Detailed => "detailed",
        }
    }
}

/// Additional reasoning payload requested from an OpenAI-compatible endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ReasoningInclude {
    #[serde(rename = "encrypted_content")]
    EncryptedContent,
}

impl ReasoningInclude {
    /// Exact OpenAI Responses wire value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EncryptedContent => "reasoning.encrypted_content",
        }
    }
}

/// Visible reasoning content shown with an assistant message.
///
/// Providers with one continuous reasoning stream use `Text`. OpenAI Responses
/// summaries use `Parts` so provider-defined summary boundaries survive
/// streaming, persistence, and rendering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ReasoningContent {
    Text(String),
    Parts(Vec<String>),
}

impl ReasoningContent {
    /// Whether every visible reasoning fragment is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Text(text) => text.is_empty(),
            Self::Parts(parts) => parts.iter().all(String::is_empty),
        }
    }

    /// Whether every visible reasoning fragment contains only whitespace.
    #[must_use]
    pub fn is_blank(&self) -> bool {
        match self {
            Self::Text(text) => text.trim().is_empty(),
            Self::Parts(parts) => parts.iter().all(|part| part.trim().is_empty()),
        }
    }
}

impl From<String> for ReasoningContent {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<Vec<String>> for ReasoningContent {
    fn from(value: Vec<String>) -> Self {
        Self::Parts(value)
    }
}

/// Opaque OpenAI Responses reasoning state returned for stateless replay.
///
/// The summary text is stored separately for display. Only the provider-issued
/// ID and encrypted payload are sent back to a Responses endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResponsesReasoningItem {
    pub id: String,
    pub encrypted_content: String,
}

/// Model metadata accepted at the configuration boundary before validation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PartialModelMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_length: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_input_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_modalities: Option<Vec<ModelModality>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_modalities: Option<Vec<ModelModality>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calling: Option<bool>,
    #[serde(
        default,
        rename = "zeroDataRetentionEnabled",
        skip_serializing_if = "Option::is_none"
    )]
    pub zero_data_retention_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_supported_efforts: Option<Vec<ReasoningEffort>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_summary: Option<ReasoningSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_include: Option<Vec<ReasoningInclude>>,
}

impl PartialModelMetadata {
    /// Resolve every mandatory value without inserting defaults.
    pub fn resolve(self) -> Result<ModelMetadata, ModelMetadataError> {
        let context_length = required(self.context_length, "context_length")?;
        let max_input_tokens = required(self.max_input_tokens, "max_input_tokens")?;
        let max_output_tokens = required(self.max_output_tokens, "max_output_tokens")?;
        ensure_positive(context_length, "context_length")?;
        ensure_positive(max_input_tokens, "max_input_tokens")?;
        ensure_positive(max_output_tokens, "max_output_tokens")?;

        if max_input_tokens.saturating_add(max_output_tokens) > context_length {
            return Err(ModelMetadataError::TokenLimitsExceedContext {
                context_length,
                max_input_tokens,
                max_output_tokens,
            });
        }

        let input_modalities = required(self.input_modalities, "input_modalities")?;
        let output_modalities = required(self.output_modalities, "output_modalities")?;
        ensure_non_empty_unique(&input_modalities, "input_modalities")?;
        ensure_non_empty_unique(&output_modalities, "output_modalities")?;

        let reasoning_supported_efforts = required(
            self.reasoning_supported_efforts,
            "reasoning_supported_efforts",
        )?;
        ensure_non_empty_strings(&reasoning_supported_efforts, "reasoning_supported_efforts")?;
        if let Some(include) = self.reasoning_include.as_ref() {
            ensure_unique(include, "reasoning_include")?;
        }

        Ok(ModelMetadata {
            context_length,
            max_input_tokens,
            max_output_tokens,
            input_modalities,
            output_modalities,
            tool_calling: required(self.tool_calling, "tool_calling")?,
            zero_data_retention_enabled: required(
                self.zero_data_retention_enabled,
                "zeroDataRetentionEnabled",
            )?,
            reasoning_supported_efforts,
            reasoning_summary: self.reasoning_summary,
            reasoning_include: self.reasoning_include,
        })
    }
}

/// Fully resolved model metadata stored by the registry and used at runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelMetadata {
    pub context_length: u32,
    pub max_input_tokens: u32,
    pub max_output_tokens: u32,
    pub input_modalities: Vec<ModelModality>,
    pub output_modalities: Vec<ModelModality>,
    pub tool_calling: bool,
    #[serde(rename = "zeroDataRetentionEnabled")]
    pub zero_data_retention_enabled: bool,
    pub reasoning_supported_efforts: Vec<ReasoningEffort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_summary: Option<ReasoningSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_include: Option<Vec<ReasoningInclude>>,
}

impl ModelMetadata {
    #[must_use]
    pub fn supports_input(&self, modality: ModelModality) -> bool {
        self.input_modalities.contains(&modality)
    }

    #[must_use]
    pub fn supports_output(&self, modality: ModelModality) -> bool {
        self.output_modalities.contains(&modality)
    }
}

impl From<&ModelMetadata> for PartialModelMetadata {
    fn from(metadata: &ModelMetadata) -> Self {
        Self {
            context_length: Some(metadata.context_length),
            max_input_tokens: Some(metadata.max_input_tokens),
            max_output_tokens: Some(metadata.max_output_tokens),
            input_modalities: Some(metadata.input_modalities.clone()),
            output_modalities: Some(metadata.output_modalities.clone()),
            tool_calling: Some(metadata.tool_calling),
            zero_data_retention_enabled: Some(metadata.zero_data_retention_enabled),
            reasoning_supported_efforts: Some(metadata.reasoning_supported_efforts.clone()),
            reasoning_summary: metadata.reasoning_summary,
            reasoning_include: metadata.reasoning_include.clone(),
        }
    }
}

impl From<ModelMetadata> for PartialModelMetadata {
    fn from(metadata: ModelMetadata) -> Self {
        Self::from(&metadata)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelMetadataError {
    #[error("missing mandatory model metadata field `{0}`")]
    MissingField(&'static str),
    #[error("model metadata field `{0}` must be greater than zero")]
    ZeroValue(&'static str),
    #[error(
        "max_input_tokens ({max_input_tokens}) + max_output_tokens ({max_output_tokens}) exceeds context_length ({context_length})"
    )]
    TokenLimitsExceedContext {
        context_length: u32,
        max_input_tokens: u32,
        max_output_tokens: u32,
    },
    #[error("model metadata field `{0}` must not be empty")]
    EmptyList(&'static str),
    #[error("model metadata field `{0}` contains duplicate values")]
    DuplicateValues(&'static str),
    #[error("model metadata field `{0}` contains an empty string")]
    EmptyString(&'static str),
}

fn required<T>(value: Option<T>, field: &'static str) -> Result<T, ModelMetadataError> {
    value.ok_or(ModelMetadataError::MissingField(field))
}

fn ensure_positive(value: u32, field: &'static str) -> Result<(), ModelMetadataError> {
    if value == 0 {
        return Err(ModelMetadataError::ZeroValue(field));
    }
    Ok(())
}

fn ensure_non_empty_unique<T>(values: &[T], field: &'static str) -> Result<(), ModelMetadataError>
where
    T: Eq + std::hash::Hash,
{
    if values.is_empty() {
        return Err(ModelMetadataError::EmptyList(field));
    }
    ensure_unique(values, field)
}

fn ensure_non_empty_strings(
    values: &[ReasoningEffort],
    field: &'static str,
) -> Result<(), ModelMetadataError> {
    if values.is_empty() {
        return Err(ModelMetadataError::EmptyList(field));
    }
    if values.iter().any(|value| value.as_str().is_empty()) {
        return Err(ModelMetadataError::EmptyString(field));
    }
    Ok(())
}

fn ensure_unique<T>(values: &[T], field: &'static str) -> Result<(), ModelMetadataError>
where
    T: Eq + std::hash::Hash,
{
    let mut seen = HashSet::with_capacity(values.len());
    if values.iter().all(|value| seen.insert(value)) {
        return Ok(());
    }
    Err(ModelMetadataError::DuplicateValues(field))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn complete_partial() -> PartialModelMetadata {
        PartialModelMetadata {
            context_length: Some(400_000),
            max_input_tokens: Some(272_000),
            max_output_tokens: Some(128_000),
            input_modalities: Some(vec![ModelModality::Text, ModelModality::Image]),
            output_modalities: Some(vec![ModelModality::Text]),
            tool_calling: Some(true),
            zero_data_retention_enabled: Some(true),
            reasoning_supported_efforts: Some(vec!["low".into(), "ultra".into()]),
            reasoning_summary: Some(ReasoningSummary::Detailed),
            reasoning_include: Some(vec![ReasoningInclude::EncryptedContent]),
        }
    }

    #[test]
    fn resolved_model_reasoning_preserves_complete_values() {
        let resolved = ResolvedModelReasoning::try_new(
            "custom-example::model".to_string(),
            ReasoningEffort::from("low"),
        )
        .unwrap();

        assert_eq!(resolved.model_id(), "custom-example::model");
        assert_eq!(resolved.reasoning_effort().as_str(), "low");
    }

    #[test]
    fn resolved_model_reasoning_rejects_empty_values() {
        let cases = [
            (
                "",
                ReasoningEffort::from("low"),
                ResolvedModelReasoningError::EmptyModelId,
            ),
            (
                "custom-example::model",
                ReasoningEffort::from(""),
                ResolvedModelReasoningError::EmptyReasoningEffort,
            ),
        ];

        for (model_id, reasoning_effort, expected) in cases {
            let error = ResolvedModelReasoning::try_new(model_id.to_string(), reasoning_effort)
                .unwrap_err();
            assert_eq!(error, expected);
        }
    }

    #[test]
    fn resolve_preserves_complete_reasoning_metadata() {
        let resolved = complete_partial().resolve().unwrap();
        assert_eq!(resolved.context_length, 400_000);
        assert_eq!(resolved.input_modalities, vec![
            ModelModality::Text,
            ModelModality::Image
        ]);
        assert_eq!(
            resolved
                .reasoning_supported_efforts
                .iter()
                .map(ReasoningEffort::as_str)
                .collect::<Vec<_>>(),
            vec!["low", "ultra"]
        );
        assert_eq!(resolved.reasoning_summary, Some(ReasoningSummary::Detailed));
        assert_eq!(
            resolved.reasoning_include,
            Some(vec![ReasoningInclude::EncryptedContent])
        );
    }

    #[test]
    fn resolve_rejects_invalid_metadata() {
        let cases = [
            (
                PartialModelMetadata::default(),
                ModelMetadataError::MissingField("context_length"),
            ),
            (
                {
                    let mut metadata = complete_partial();
                    metadata.input_modalities = Some(Vec::new());
                    metadata
                },
                ModelMetadataError::EmptyList("input_modalities"),
            ),
            (
                {
                    let mut metadata = complete_partial();
                    metadata.context_length = Some(399_999);
                    metadata
                },
                ModelMetadataError::TokenLimitsExceedContext {
                    context_length: 399_999,
                    max_input_tokens: 272_000,
                    max_output_tokens: 128_000,
                },
            ),
            (
                {
                    let mut metadata = complete_partial();
                    metadata.reasoning_supported_efforts = Some(Vec::new());
                    metadata
                },
                ModelMetadataError::EmptyList("reasoning_supported_efforts"),
            ),
            (
                {
                    let mut metadata = complete_partial();
                    metadata.reasoning_supported_efforts = Some(vec!["".into()]);
                    metadata
                },
                ModelMetadataError::EmptyString("reasoning_supported_efforts"),
            ),
            (
                {
                    let mut metadata = complete_partial();
                    metadata.reasoning_supported_efforts =
                        Some(vec!["low".into(), "".into(), "high".into()]);
                    metadata
                },
                ModelMetadataError::EmptyString("reasoning_supported_efforts"),
            ),
        ];

        for (metadata, expected) in cases {
            assert_eq!(metadata.resolve().unwrap_err(), expected);
        }
    }

    #[test]
    fn reasoning_efforts_preserve_duplicates_and_order() {
        let mut partial = complete_partial();
        partial.reasoning_supported_efforts =
            Some(vec!["high".into(), "low".into(), "high".into()]);
        let resolved = partial.resolve().unwrap();
        assert_eq!(
            resolved
                .reasoning_supported_efforts
                .iter()
                .map(ReasoningEffort::as_str)
                .collect::<Vec<_>>(),
            vec!["high", "low", "high"]
        );
    }

    #[test]
    fn reasoning_include_uses_config_value_and_wire_prefix() {
        let json = serde_json::to_value(ReasoningInclude::EncryptedContent).unwrap();
        assert_eq!(json, serde_json::json!("encrypted_content"));
        assert_eq!(
            ReasoningInclude::EncryptedContent.as_str(),
            "reasoning.encrypted_content"
        );
    }

    #[test]
    fn zero_data_retention_uses_vscode_field_name() {
        let json = serde_json::to_value(PartialModelMetadata {
            zero_data_retention_enabled: Some(false),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            json.get("zeroDataRetentionEnabled"),
            Some(&serde_json::json!(false))
        );
        assert!(json.get("zero_data_retention_enabled").is_none());
    }

    #[test]
    fn resolved_metadata_converts_to_complete_partial_record() {
        let resolved = complete_partial().resolve().unwrap();
        let round_trip = PartialModelMetadata::from(&resolved).resolve().unwrap();
        assert_eq!(round_trip, resolved);
    }
}
