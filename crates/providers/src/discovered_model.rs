//! Partial model discovery and strict registry resolution.

use std::collections::{HashMap, HashSet};

use chelix_common::{ModelConfigMap, ModelMetadata, PartialModelMetadata};

/// A model returned by a provider catalog before config precedence and strict validation.
#[derive(Debug, Clone)]
pub struct DiscoveredModel {
    pub id: String,
    pub display_name: String,
    /// Unix timestamp from the provider API.
    pub created_at: Option<i64>,
    /// Explicit provider recommendation; never inferred from the model ID.
    pub recommended: bool,
    pub metadata: PartialModelMetadata,
}

impl DiscoveredModel {
    #[must_use]
    pub fn new(id: impl Into<String>, display_name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            display_name: display_name.into(),
            created_at: None,
            recommended: false,
            metadata: PartialModelMetadata::default(),
        }
    }

    #[must_use]
    pub fn with_created_at(mut self, created_at: Option<i64>) -> Self {
        self.created_at = created_at;
        self
    }

    #[must_use]
    pub fn with_recommended(mut self, recommended: bool) -> Self {
        self.recommended = recommended;
        self
    }

    #[must_use]
    pub fn with_metadata(mut self, metadata: PartialModelMetadata) -> Self {
        self.metadata = metadata;
        self
    }
}

/// A complete model record accepted by the registry.
#[derive(Debug, Clone)]
pub struct ResolvedModel {
    pub id: String,
    pub display_name: String,
    pub created_at: Option<i64>,
    pub recommended: bool,
    pub metadata: ModelMetadata,
}

/// Apply config precedence and allowlist semantics, then reject incomplete records.
///
/// A non-empty config map is an ordered allowlist. An empty map accepts every
/// discovered model that resolves successfully.
#[must_use]
pub fn resolve_models(
    configured: &ModelConfigMap,
    discovered: Vec<DiscoveredModel>,
) -> Vec<ResolvedModel> {
    if configured.is_empty() {
        return discovered
            .into_iter()
            .filter_map(resolve_discovered_model)
            .collect();
    }

    let mut discovered_by_id: HashMap<String, DiscoveredModel> = discovered
        .into_iter()
        .map(|model| (model.id.clone(), model))
        .collect();

    configured
        .iter()
        .filter_map(|(model_id, configured_metadata)| {
            let discovered = discovered_by_id.remove(model_id);
            let (display_name, created_at, recommended, discovered_metadata) = discovered
                .map(|model| {
                    (
                        model.display_name,
                        model.created_at,
                        model.recommended,
                        model.metadata,
                    )
                })
                .unwrap_or_else(|| {
                    (
                        model_id.clone(),
                        None,
                        false,
                        PartialModelMetadata::default(),
                    )
                });
            let merged = configured_metadata
                .clone()
                .with_fallback(discovered_metadata);
            match merged.resolve() {
                Ok(metadata) => Some(ResolvedModel {
                    id: model_id.clone(),
                    display_name,
                    created_at,
                    recommended,
                    metadata,
                }),
                Err(error) => {
                    tracing::debug!(
                        model = %model_id,
                        error = %error,
                        "excluding unresolved configured model"
                    );
                    None
                },
            }
        })
        .collect()
}

fn resolve_discovered_model(model: DiscoveredModel) -> Option<ResolvedModel> {
    match model.metadata.resolve() {
        Ok(metadata) => Some(ResolvedModel {
            id: model.id,
            display_name: model.display_name,
            created_at: model.created_at,
            recommended: model.recommended,
            metadata,
        }),
        Err(error) => {
            tracing::debug!(model = %model.id, error = %error, "excluding unresolved discovered model");
            None
        },
    }
}

/// Keep the first discovered record for each model ID.
#[must_use]
pub(crate) fn deduplicate_discovered(models: Vec<DiscoveredModel>) -> Vec<DiscoveredModel> {
    let mut seen = HashSet::new();
    models
        .into_iter()
        .filter(|model| seen.insert(model.id.clone()))
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use {
        super::*,
        chelix_common::{PartialReasoningMetadata, ReasoningEffort},
    };

    fn complete_metadata(context_length: u32) -> PartialModelMetadata {
        PartialModelMetadata {
            context_length: Some(context_length),
            max_input_tokens: Some(context_length - 1_000),
            max_output_tokens: Some(1_000),
            reasoning: Some(PartialReasoningMetadata {
                supported_efforts: Some(Vec::new()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn nonempty_config_is_ordered_allowlist_and_config_wins() {
        let mut configured = ModelConfigMap::new();
        configured.insert("allowed-b".into(), PartialModelMetadata {
            context_length: Some(20_000),
            ..Default::default()
        });
        configured.insert("allowed-a".into(), PartialModelMetadata::default());

        let discovered = vec![
            DiscoveredModel::new("allowed-a", "Allowed A").with_metadata(complete_metadata(10_000)),
            DiscoveredModel::new("excluded", "Excluded").with_metadata(complete_metadata(10_000)),
            DiscoveredModel::new("allowed-b", "Allowed B").with_metadata(complete_metadata(10_000)),
        ];

        let resolved = resolve_models(&configured, discovered);
        let ids: Vec<&str> = resolved.iter().map(|model| model.id.as_str()).collect();
        assert_eq!(ids, vec!["allowed-b", "allowed-a"]);
        assert_eq!(resolved[0].metadata.context_length, 20_000);
    }

    #[test]
    fn empty_config_accepts_all_complete_discovered_models() {
        let resolved = resolve_models(&ModelConfigMap::new(), vec![
            DiscoveredModel::new("complete-a", "Complete A")
                .with_metadata(complete_metadata(10_000)),
            DiscoveredModel::new("complete-b", "Complete B")
                .with_metadata(complete_metadata(20_000)),
        ]);
        assert_eq!(resolved.len(), 2);
    }

    #[test]
    fn incomplete_models_are_excluded() {
        let resolved = resolve_models(&ModelConfigMap::new(), vec![DiscoveredModel::new(
            "incomplete",
            "Incomplete",
        )]);
        assert!(resolved.is_empty());
    }

    #[test]
    fn complete_config_only_model_is_accepted() {
        let mut configured = ModelConfigMap::new();
        configured.insert("config-only".into(), complete_metadata(10_000));
        let resolved = resolve_models(&configured, Vec::new());
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].id, "config-only");
    }

    #[test]
    fn reasoning_true_without_known_efforts_remains_unresolved() {
        let metadata = PartialModelMetadata {
            context_length: Some(10_000),
            max_input_tokens: Some(9_000),
            max_output_tokens: Some(1_000),
            reasoning: Some(PartialReasoningMetadata {
                supported_efforts: None,
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = resolve_models(&ModelConfigMap::new(), vec![
            DiscoveredModel::new("unknown-efforts", "Unknown Efforts").with_metadata(metadata),
        ]);
        assert!(resolved.is_empty());
    }

    #[test]
    fn configured_efforts_supplement_discovery() {
        let mut configured = ModelConfigMap::new();
        configured.insert("reasoning".into(), PartialModelMetadata {
            reasoning: Some(PartialReasoningMetadata {
                supported_efforts: Some(vec!["max".into(), "ultra".into()]),
                ..Default::default()
            }),
            ..Default::default()
        });
        let mut discovered_metadata = complete_metadata(10_000);
        discovered_metadata.reasoning = None;
        let resolved = resolve_models(&configured, vec![
            DiscoveredModel::new("reasoning", "Reasoning").with_metadata(discovered_metadata),
        ]);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].metadata.reasoning.supported_efforts, vec![
            ReasoningEffort::from("max"),
            ReasoningEffort::from("ultra")
        ]);
    }
}
