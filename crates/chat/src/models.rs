//! Model service and disabled model store.

use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
};

use {
    async_trait::async_trait,
    serde::{Deserialize, Serialize},
    serde_json::Value,
    tokio::sync::{OnceCell, RwLock},
    tracing::{debug, info},
};

use {
    chelix_providers::{ProviderRegistry, model_id::raw_model_id},
    chelix_service_traits::{ModelService, ServiceError, ServiceResult},
};

use crate::{
    runtime::ChatRuntime,
    types::{BroadcastOpts, broadcast, normalize_model_key},
};

// ── Disabled Models Store ────────────────────────────────────────────────────

/// Persistent store for manually disabled model IDs.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct DisabledModelsStore {
    #[serde(default)]
    pub disabled: HashSet<String>,
}

impl DisabledModelsStore {
    fn config_path() -> Option<PathBuf> {
        chelix_config::config_dir().map(|d| d.join("disabled-models.json"))
    }

    /// Load disabled models from config file.
    pub fn load() -> crate::error::Result<Self> {
        let path = Self::config_path().ok_or(crate::error::Error::NoConfigDirectory)?;
        let absolute_path = std::path::absolute(&path).unwrap_or_else(|_| path.clone());
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            },
            Err(error) => {
                return Err(crate::error::Error::external(
                    format!("failed to read {}", absolute_path.display()),
                    error,
                ));
            },
        };
        serde_json::from_str(&content).map_err(|error| {
            crate::error::Error::external(
                format!("failed to parse {}", absolute_path.display()),
                error,
            )
        })
    }

    /// Save disabled models to config file.
    pub fn save(&self) -> crate::error::Result<()> {
        let path = Self::config_path().ok_or(crate::error::Error::NoConfigDirectory)?;
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    /// Disable a model by ID.
    pub fn disable(&mut self, model_id: &str) -> bool {
        self.disabled.insert(model_id.to_string())
    }

    /// Enable a model by ID (remove from disabled set).
    pub fn enable(&mut self, model_id: &str) -> bool {
        self.disabled.remove(model_id)
    }

    /// Check if a model is disabled.
    pub fn is_disabled(&self, model_id: &str) -> bool {
        self.disabled.contains(model_id)
    }
}

// ── LiveModelService ────────────────────────────────────────────────────────

pub struct LiveModelService {
    providers: Arc<RwLock<ProviderRegistry>>,
    disabled: Arc<RwLock<DisabledModelsStore>>,
    state: Arc<OnceCell<Arc<dyn ChatRuntime>>>,
    priority_models: Arc<RwLock<Vec<String>>>,
}

impl LiveModelService {
    pub fn new(
        providers: Arc<RwLock<ProviderRegistry>>,
        disabled: Arc<RwLock<DisabledModelsStore>>,
        priority_models: Vec<String>,
    ) -> Self {
        Self {
            providers,
            disabled,
            state: Arc::new(OnceCell::new()),
            priority_models: Arc::new(RwLock::new(priority_models)),
        }
    }

    /// Shared handle to the priority models list. Pass this to services
    /// that need to update model ordering at runtime (e.g. `save_model`).
    pub fn priority_models_handle(&self) -> Arc<RwLock<Vec<String>>> {
        Arc::clone(&self.priority_models)
    }

    fn build_priority_order(models: &[String]) -> HashMap<String, usize> {
        let mut order = HashMap::new();
        for (idx, model) in models.iter().enumerate() {
            let key = normalize_model_key(model);
            if !key.is_empty() {
                let _ = order.entry(key).or_insert(idx);
            }
        }
        order
    }

    fn priority_rank(order: &HashMap<String, usize>, model: &chelix_providers::ModelInfo) -> usize {
        let full = normalize_model_key(&model.id);
        if let Some(rank) = order.get(&full) {
            return *rank;
        }
        let raw = normalize_model_key(raw_model_id(&model.id));
        if let Some(rank) = order.get(&raw) {
            return *rank;
        }
        usize::MAX
    }

    fn prioritize_models<'a>(
        order: &HashMap<String, usize>,
        models: impl Iterator<Item = &'a chelix_providers::ModelInfo>,
    ) -> Vec<&'a chelix_providers::ModelInfo> {
        let mut ordered: Vec<(usize, &'a chelix_providers::ModelInfo)> =
            models.enumerate().collect();
        ordered.sort_by(|(idx_a, a), (idx_b, b)| {
            let rank_a = Self::priority_rank(order, a);
            let rank_b = Self::priority_rank(order, b);
            // Preferred (rank != MAX) first, then non-preferred
            let bucket_a = if rank_a == usize::MAX {
                1u8
            } else {
                0
            };
            let bucket_b = if rank_b == usize::MAX {
                1u8
            } else {
                0
            };
            bucket_a
                .cmp(&bucket_b)
                .then_with(|| {
                    if bucket_a == 0 {
                        rank_a.cmp(&rank_b)
                    } else {
                        Ordering::Equal
                    }
                })
                .then_with(|| a.id.to_lowercase().cmp(&b.id.to_lowercase()))
                .then_with(|| idx_a.cmp(idx_b))
        });
        ordered.into_iter().map(|(_, model)| model).collect()
    }

    async fn priority_order(&self) -> HashMap<String, usize> {
        let list = self.priority_models.read().await;
        Self::build_priority_order(&list)
    }

    /// Set the gateway state reference for broadcasting model updates.
    pub fn set_state(&self, state: Arc<dyn ChatRuntime>) {
        let _ = self.state.set(state);
    }

    async fn broadcast_model_visibility_update(&self, model_id: &str, disabled: bool) {
        if let Some(state) = self.state.get() {
            broadcast(
                state,
                "models.updated",
                serde_json::json!({
                    "modelId": model_id,
                    "disabled": disabled,
                }),
                BroadcastOpts::default(),
            )
            .await;
        }
    }

    fn model_value(
        model: &chelix_providers::ModelInfo,
        preferred: bool,
        disabled: bool,
    ) -> Result<Value, serde_json::Error> {
        let mut value = serde_json::to_value(model)?;
        if let Some(object) = value.as_object_mut() {
            object.insert("preferred".into(), Value::Bool(preferred));
            object.insert("disabled".into(), Value::Bool(disabled));
        }
        Ok(value)
    }
}

#[async_trait]
impl ModelService for LiveModelService {
    async fn list(&self) -> ServiceResult {
        let reg = self.providers.read().await;
        let disabled = self.disabled.read().await;
        let order = self.priority_order().await;
        let all_models = reg.list_models();

        let prioritized = Self::prioritize_models(
            &order,
            all_models
                .iter()
                .filter(|model| model.supports_text_chat())
                .filter(|m| !disabled.is_disabled(&m.id)),
        );
        debug!(model_count = prioritized.len(), "models.list response");
        let models: Vec<_> = prioritized
            .iter()
            .copied()
            .map(|m| {
                let preferred = Self::priority_rank(&order, m) != usize::MAX;
                Self::model_value(m, preferred, false)
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(ServiceError::message)?;
        Ok(serde_json::json!(models))
    }

    async fn list_all(&self) -> ServiceResult {
        let reg = self.providers.read().await;
        let disabled = self.disabled.read().await;
        let order = self.priority_order().await;
        let all_models = reg.list_models();
        let prioritized = Self::prioritize_models(
            &order,
            all_models.iter().filter(|model| model.supports_text_chat()),
        );
        info!(model_count = prioritized.len(), "models.list_all response");
        let models: Vec<_> = prioritized
            .iter()
            .copied()
            .map(|m| {
                let preferred = Self::priority_rank(&order, m) != usize::MAX;
                Self::model_value(m, preferred, disabled.is_disabled(&m.id))
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(ServiceError::message)?;
        Ok(serde_json::json!(models))
    }

    async fn resolve_model_reasoning(
        &self,
        model: &str,
        reasoning_effort: Option<&chelix_common::ReasoningEffort>,
    ) -> Result<chelix_common::ReasoningEffort, ServiceError> {
        let models = self.list().await?;
        let models = models
            .as_array()
            .ok_or_else(|| ServiceError::message("models.list returned an invalid response"))?;
        if !models
            .iter()
            .any(|entry| entry.get("id").and_then(Value::as_str) == Some(model))
        {
            return Err(ServiceError::message(format!(
                "model '{model}' not found in chat model registry"
            )));
        }

        self.providers
            .read()
            .await
            .resolve_model_reasoning(Some(model), reasoning_effort)
            .map(|resolved| resolved.model_reasoning().reasoning_effort().clone())
            .map_err(ServiceError::message)
    }

    async fn disable(&self, params: Value) -> ServiceResult {
        let model_id = params
            .get("modelId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing 'modelId' parameter".to_string())?;

        info!(model = %model_id, "disabling model");

        let mut disabled = self.disabled.write().await;
        disabled.disable(model_id);
        disabled
            .save()
            .map_err(|e| format!("failed to save: {e}"))?;
        drop(disabled);

        self.broadcast_model_visibility_update(model_id, true).await;

        Ok(serde_json::json!({
            "ok": true,
            "modelId": model_id,
        }))
    }

    async fn enable(&self, params: Value) -> ServiceResult {
        let model_id = params
            .get("modelId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing 'modelId' parameter".to_string())?;

        info!(model = %model_id, "enabling model");

        let mut disabled = self.disabled.write().await;
        disabled.enable(model_id);
        disabled
            .save()
            .map_err(|e| format!("failed to save: {e}"))?;
        drop(disabled);

        self.broadcast_model_visibility_update(model_id, false)
            .await;

        Ok(serde_json::json!({
            "ok": true,
            "modelId": model_id,
        }))
    }
}
