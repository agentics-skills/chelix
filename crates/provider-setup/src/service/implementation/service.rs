//! `LiveProviderSetupService` — runtime implementation of
//! `ProviderSetupService`.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use secrecy::Secret;

use {async_trait::async_trait, serde_json::Value, tokio::sync::RwLock};

use {
    chelix_config::{AgentsConfig, schema::ProvidersConfig},
    chelix_providers::ProviderRegistry,
    chelix_service_traits::{ProviderSetupService, ServiceError, ServiceResult},
};

pub use super::support::ErrorParser;
use super::support::default_error_parser;

// ── LiveProviderSetupService ───────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderConfigPersistence {
    Filesystem,
    MemoryOnly,
}

pub struct LiveProviderSetupService {
    pub(crate) registry: Arc<RwLock<ProviderRegistry>>,
    pub(crate) config: Arc<Mutex<ProvidersConfig>>,
    pub(crate) config_persistence: ProviderConfigPersistence,
    /// Shared priority models list from `LiveModelService`. Updated when the
    /// ordered model selection changes so the dropdown reflects that order.
    pub(crate) priority_models: Option<Arc<RwLock<Vec<String>>>>,
    pub(crate) agents_config: Option<Arc<RwLock<AgentsConfig>>>,
    /// Static env overrides (for example config `[env]`) used when resolving
    /// provider credentials without mutating the process environment.
    pub(crate) env_overrides: HashMap<String, String>,
    /// Injected error parser for interpreting provider API errors.
    pub(crate) error_parser: ErrorParser,
}

impl LiveProviderSetupService {
    pub fn new(
        registry: Arc<RwLock<ProviderRegistry>>,
        config: ProvidersConfig,
        config_persistence: ProviderConfigPersistence,
    ) -> Self {
        Self {
            registry,
            config: Arc::new(Mutex::new(config)),
            config_persistence,
            priority_models: None,
            agents_config: None,
            env_overrides: HashMap::new(),
            error_parser: default_error_parser,
        }
    }

    pub fn with_env_overrides(mut self, env_overrides: HashMap<String, String>) -> Self {
        self.env_overrides = env_overrides;
        self
    }

    pub fn with_agents_config(mut self, agents_config: Arc<RwLock<AgentsConfig>>) -> Self {
        self.agents_config = Some(agents_config);
        self
    }

    /// Set a custom error parser for interpreting provider API errors.
    pub fn with_error_parser(mut self, parser: ErrorParser) -> Self {
        self.error_parser = parser;
        self
    }

    /// Wire the shared priority models handle from `LiveModelService` so model
    /// selection changes can update dropdown ordering at runtime.
    pub fn set_priority_models(&mut self, handle: Arc<RwLock<Vec<String>>>) {
        self.priority_models = Some(handle);
    }

    pub(crate) fn config_snapshot(&self) -> ProvidersConfig {
        self.config
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub(crate) fn effective_config(&self) -> ServiceResult<ProvidersConfig> {
        Ok(self.config_snapshot())
    }

    pub(crate) fn prospective_config_with_saved_update(
        &self,
        provider: &str,
        api_key: Option<&str>,
        base_url: Option<&str>,
        enabled: Option<bool>,
    ) -> ServiceResult<ProvidersConfig> {
        let mut candidate = self.config_snapshot();
        let entry = candidate.providers.entry(provider.to_string()).or_default();
        if let Some(enabled) = enabled {
            entry.enabled = enabled;
        }
        if let Some(api_key) = api_key {
            entry.api_key = Some(Secret::new(api_key.to_string()));
        }
        if let Some(base_url) = base_url {
            entry.base_url = (!base_url.is_empty()).then(|| base_url.to_string());
        }
        Ok(candidate)
    }

    pub(crate) fn prospective_config_without_saved_provider(
        &self,
        provider: &str,
    ) -> ServiceResult<ProvidersConfig> {
        let mut candidate = self.config_snapshot();
        let entry = candidate.providers.entry(provider.to_string()).or_default();
        entry.api_key = None;
        entry.enabled = false;
        Ok(candidate)
    }

    pub(crate) fn build_registry(
        &self,
        config: &ProvidersConfig,
    ) -> ServiceResult<ProviderRegistry> {
        ProviderRegistry::from_config(config, &self.env_overrides).map_err(ServiceError::message)
    }
}

#[async_trait]
impl ProviderSetupService for LiveProviderSetupService {
    async fn available(&self) -> ServiceResult {
        self.available_inner().await
    }

    async fn save_key(&self, params: Value) -> ServiceResult {
        self.save_key_inner(params).await
    }

    async fn remove_key(&self, params: Value) -> ServiceResult {
        self.remove_key_inner(params).await
    }

    async fn set_model_preferences(&self, params: Value) -> ServiceResult {
        self.set_model_preferences_inner(params).await
    }

    async fn upsert_model(&self, params: Value) -> ServiceResult {
        self.upsert_model_inner(params).await
    }

    async fn delete_model(&self, params: Value) -> ServiceResult {
        self.delete_model_inner(params).await
    }

    async fn upsert_openai_compatible(&self, params: Value) -> ServiceResult {
        self.upsert_openai_compatible_inner(params).await
    }

    async fn delete_openai_compatible(&self, params: Value) -> ServiceResult {
        self.delete_openai_compatible_inner(params).await
    }
}
