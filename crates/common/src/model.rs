//! Canonical model metadata shared by configuration, discovery, and runtime.

use {
    indexmap::IndexMap,
    serde::{Deserialize, Serialize},
    std::collections::HashSet,
};

/// Ordered provider model allowlist with per-model metadata overrides.
pub type ModelConfigMap = IndexMap<String, PartialModelMetadata>;

/// Provider-defined reasoning/thinking effort level supported by a model.
///
/// Values come from configured or discovered model metadata and are
/// intentionally not restricted to a hard-coded vocabulary.
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

/// Additional reasoning payload requested from an OpenAI-compatible endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ReasoningInclude {
    #[serde(rename = "reasoning.encrypted_content")]
    EncryptedContent,
}

/// Partial reasoning metadata supplied by configuration or model discovery.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PartialReasoningMetadata {
    /// `Some([])` explicitly identifies a non-reasoning model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supported_efforts: Option<Vec<ReasoningEffort>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<ReasoningSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<ReasoningInclude>>,
}

impl PartialReasoningMetadata {
    /// Fill absent fields from lower-priority metadata.
    #[must_use]
    pub fn with_fallback(self, fallback: Self) -> Self {
        Self {
            supported_efforts: self.supported_efforts.or(fallback.supported_efforts),
            summary: self.summary.or(fallback.summary),
            include: self.include.or(fallback.include),
        }
    }
}

/// Partial model metadata supplied by configuration or model discovery.
///
/// Configuration is merged over discovery field-by-field before resolution.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streaming: Option<bool>,
    #[serde(
        default,
        rename = "zeroDataRetentionEnabled",
        skip_serializing_if = "Option::is_none"
    )]
    pub zero_data_retention_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<PartialReasoningMetadata>,
}

impl PartialModelMetadata {
    /// Fill absent fields from lower-priority metadata.
    #[must_use]
    pub fn with_fallback(self, fallback: Self) -> Self {
        let reasoning = match (self.reasoning, fallback.reasoning) {
            (Some(preferred), Some(fallback)) => Some(preferred.with_fallback(fallback)),
            (Some(preferred), None) => Some(preferred),
            (None, fallback) => fallback,
        };

        Self {
            context_length: self.context_length.or(fallback.context_length),
            max_input_tokens: self.max_input_tokens.or(fallback.max_input_tokens),
            max_output_tokens: self.max_output_tokens.or(fallback.max_output_tokens),
            input_modalities: self.input_modalities.or(fallback.input_modalities),
            output_modalities: self.output_modalities.or(fallback.output_modalities),
            tool_calling: self.tool_calling.or(fallback.tool_calling),
            streaming: self.streaming.or(fallback.streaming),
            zero_data_retention_enabled: self
                .zero_data_retention_enabled
                .or(fallback.zero_data_retention_enabled),
            reasoning,
        }
    }

    /// Resolve mandatory values and apply the agreed optional defaults.
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

        let partial_reasoning = self.reasoning.ok_or(ModelMetadataError::MissingField(
            "reasoning.supported_efforts",
        ))?;
        let supported_efforts = required(
            partial_reasoning.supported_efforts,
            "reasoning.supported_efforts",
        )?;
        ensure_unique(&supported_efforts, "reasoning.supported_efforts")?;

        let reasoning = if supported_efforts.is_empty() {
            if partial_reasoning.summary.is_some()
                || partial_reasoning
                    .include
                    .as_ref()
                    .is_some_and(|v| !v.is_empty())
            {
                return Err(ModelMetadataError::ReasoningOptionsOnUnsupportedModel);
            }
            ModelReasoningMetadata {
                supported_efforts,
                summary: None,
                include: Vec::new(),
            }
        } else {
            let include = partial_reasoning
                .include
                .unwrap_or_else(|| vec![ReasoningInclude::EncryptedContent]);
            ensure_unique(&include, "reasoning.include")?;
            ModelReasoningMetadata {
                supported_efforts,
                summary: Some(
                    partial_reasoning
                        .summary
                        .unwrap_or(ReasoningSummary::Detailed),
                ),
                include,
            }
        };

        let input_modalities = self
            .input_modalities
            .unwrap_or_else(|| vec![ModelModality::Text, ModelModality::Image]);
        let output_modalities = self
            .output_modalities
            .unwrap_or_else(|| vec![ModelModality::Text]);
        ensure_non_empty_unique(&input_modalities, "input_modalities")?;
        ensure_non_empty_unique(&output_modalities, "output_modalities")?;

        Ok(ModelMetadata {
            context_length,
            max_input_tokens,
            max_output_tokens,
            input_modalities,
            output_modalities,
            tool_calling: self.tool_calling.unwrap_or(true),
            streaming: self.streaming.unwrap_or(true),
            zero_data_retention_enabled: self.zero_data_retention_enabled.unwrap_or(true),
            reasoning,
        })
    }
}

/// Fully resolved model metadata stored by the registry and used at runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelMetadata {
    pub context_length: u32,
    pub max_input_tokens: u32,
    pub max_output_tokens: u32,
    pub input_modalities: Vec<ModelModality>,
    pub output_modalities: Vec<ModelModality>,
    pub tool_calling: bool,
    pub streaming: bool,
    #[serde(rename = "zeroDataRetentionEnabled")]
    pub zero_data_retention_enabled: bool,
    pub reasoning: ModelReasoningMetadata,
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

    #[must_use]
    pub fn supports_reasoning(&self) -> bool {
        !self.reasoning.supported_efforts.is_empty()
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
            streaming: Some(metadata.streaming),
            zero_data_retention_enabled: Some(metadata.zero_data_retention_enabled),
            reasoning: Some(PartialReasoningMetadata {
                supported_efforts: Some(metadata.reasoning.supported_efforts.clone()),
                summary: metadata.reasoning.summary,
                include: Some(metadata.reasoning.include.clone()),
            }),
        }
    }
}

impl From<ModelMetadata> for PartialModelMetadata {
    fn from(metadata: ModelMetadata) -> Self {
        Self::from(&metadata)
    }
}

/// Fully resolved reasoning metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelReasoningMetadata {
    pub supported_efforts: Vec<ReasoningEffort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<ReasoningSummary>,
    pub include: Vec<ReasoningInclude>,
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
    #[error("reasoning summary/include cannot be set when supported_efforts is empty")]
    ReasoningOptionsOnUnsupportedModel,
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
            reasoning: Some(PartialReasoningMetadata {
                supported_efforts: Some(vec!["low".into(), "ultra".into()]),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn merge_uses_preferred_fields_and_supplements_missing_fields() {
        let preferred = PartialModelMetadata {
            context_length: Some(400_000),
            tool_calling: Some(false),
            reasoning: Some(PartialReasoningMetadata {
                summary: Some(ReasoningSummary::Concise),
                ..Default::default()
            }),
            ..Default::default()
        };
        let fallback = PartialModelMetadata {
            context_length: Some(200_000),
            max_input_tokens: Some(272_000),
            tool_calling: Some(true),
            reasoning: Some(PartialReasoningMetadata {
                supported_efforts: Some(vec!["ultra".into()]),
                summary: Some(ReasoningSummary::Detailed),
                include: Some(vec![ReasoningInclude::EncryptedContent]),
            }),
            ..Default::default()
        };

        let merged = preferred.with_fallback(fallback);
        assert_eq!(merged.context_length, Some(400_000));
        assert_eq!(merged.max_input_tokens, Some(272_000));
        assert_eq!(merged.tool_calling, Some(false));
        let reasoning = merged.reasoning.unwrap();
        assert_eq!(reasoning.supported_efforts, Some(vec!["ultra".into()]));
        assert_eq!(reasoning.summary, Some(ReasoningSummary::Concise));
        assert_eq!(
            reasoning.include,
            Some(vec![ReasoningInclude::EncryptedContent])
        );
    }

    #[test]
    fn resolve_applies_optional_defaults() {
        let resolved = complete_partial().resolve().unwrap();
        assert_eq!(resolved.input_modalities, vec![
            ModelModality::Text,
            ModelModality::Image
        ]);
        assert_eq!(resolved.output_modalities, vec![ModelModality::Text]);
        assert!(resolved.tool_calling);
        assert!(resolved.streaming);
        assert!(resolved.zero_data_retention_enabled);
        assert_eq!(resolved.reasoning.summary, Some(ReasoningSummary::Detailed));
        assert_eq!(resolved.reasoning.include, vec![
            ReasoningInclude::EncryptedContent
        ]);
    }

    #[test]
    fn resolve_accepts_explicit_non_reasoning_model() {
        let mut partial = complete_partial();
        partial.reasoning = Some(PartialReasoningMetadata {
            supported_efforts: Some(Vec::new()),
            ..Default::default()
        });

        let resolved = partial.resolve().unwrap();
        assert!(!resolved.supports_reasoning());
        assert_eq!(resolved.reasoning.summary, None);
        assert!(resolved.reasoning.include.is_empty());
    }

    #[test]
    fn resolve_rejects_missing_mandatory_metadata() {
        let error = PartialModelMetadata::default().resolve().unwrap_err();
        assert_eq!(error, ModelMetadataError::MissingField("context_length"));

        let mut partial = complete_partial();
        partial.reasoning = None;
        let error = partial.resolve().unwrap_err();
        assert_eq!(
            error,
            ModelMetadataError::MissingField("reasoning.supported_efforts")
        );
    }

    #[test]
    fn resolve_rejects_inconsistent_token_limits() {
        let mut partial = complete_partial();
        partial.context_length = Some(399_999);
        assert!(matches!(
            partial.resolve(),
            Err(ModelMetadataError::TokenLimitsExceedContext { .. })
        ));
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
