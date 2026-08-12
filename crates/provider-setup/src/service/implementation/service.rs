//! `LiveProviderSetupService` — runtime implementation of
//! `ProviderSetupService`.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use secrecy::ExposeSecret;

use {
    async_trait::async_trait,
    serde_json::{Map, Value},
    tokio::sync::{OnceCell, RwLock},
    tracing::{error, info},
};

use {
    chelix_config::schema::ProvidersConfig,
    chelix_providers::ProviderRegistry,
    chelix_service_traits::{ProviderSetupService, ServiceResult},
};

pub use super::support::ErrorParser;
use {
    super::support::default_error_parser,
    crate::{
        SetupBroadcaster,
        config_helpers::{
            config_with_saved_keys, env_value_with_overrides, home_key_store,
            set_provider_enabled_in_config,
        },
        key_store::KeyStore,
        known_providers::KnownProvider,
    },
};

// ── LiveProviderSetupService ───────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderConfigPersistence {
    Filesystem,
    MemoryOnly,
}

pub struct LiveProviderSetupService {
    pub(crate) registry: Arc<RwLock<ProviderRegistry>>,
    pub(crate) config: Arc<Mutex<ProvidersConfig>>,
    config_persistence: ProviderConfigPersistence,
    broadcaster: Arc<OnceCell<Arc<dyn SetupBroadcaster>>>,
    pub(crate) key_store: KeyStore,
    /// When set, local-only providers are hidden from
    /// the available list because they cannot run on cloud VMs.
    pub(crate) deploy_platform: Option<String>,
    /// Shared priority models list from `LiveModelService`. Updated when the
    /// ordered model selection changes so the dropdown reflects that order.
    pub(crate) priority_models: Option<Arc<RwLock<Vec<String>>>>,
    /// Monotonic sequence used to drop stale async registry refreshes.
    registry_rebuild_seq: Arc<AtomicU64>,
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
        deploy_platform: Option<String>,
        config_persistence: ProviderConfigPersistence,
    ) -> Self {
        Self {
            registry,
            config: Arc::new(Mutex::new(config)),
            config_persistence,
            broadcaster: Arc::new(OnceCell::new()),
            key_store: KeyStore::new(),
            deploy_platform,
            priority_models: None,
            registry_rebuild_seq: Arc::new(AtomicU64::new(0)),
            env_overrides: HashMap::new(),
            error_parser: default_error_parser,
        }
    }

    pub fn with_env_overrides(mut self, env_overrides: HashMap<String, String>) -> Self {
        self.env_overrides = env_overrides;
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

    /// Set the broadcaster so validation can publish live progress events
    /// to the UI over WebSocket.
    pub fn set_broadcaster(&self, broadcaster: Arc<dyn SetupBroadcaster>) {
        let _ = self.broadcaster.set(broadcaster);
    }

    pub(crate) async fn emit_validation_progress(
        &self,
        provider: &str,
        request_id: Option<&str>,
        phase: &str,
        mut extra: Map<String, Value>,
    ) {
        let Some(broadcaster) = self.broadcaster.get() else {
            return;
        };

        let mut payload = Map::new();
        payload.insert("provider".to_string(), Value::String(provider.to_string()));
        payload.insert("phase".to_string(), Value::String(phase.to_string()));
        if let Some(id) = request_id {
            payload.insert("requestId".to_string(), Value::String(id.to_string()));
        }
        payload.append(&mut extra);

        broadcaster
            .broadcast("providers.validate.progress", Value::Object(payload))
            .await;
    }

    pub(crate) fn queue_registry_rebuild(&self, provider_name: &str, reason: &'static str) {
        let rebuild_seq = self.registry_rebuild_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let latest_seq = Arc::clone(&self.registry_rebuild_seq);
        let registry = Arc::clone(&self.registry);
        let config = Arc::clone(&self.config);
        let key_store = self.key_store.clone();
        let env_overrides = self.env_overrides.clone();
        let provider_name = provider_name.to_string();

        tokio::spawn(async move {
            let started = std::time::Instant::now();
            info!(
                provider = %provider_name,
                reason,
                rebuild_seq,
                "provider registry async rebuild started"
            );

            let effective = {
                let base = config.lock().unwrap_or_else(|e| e.into_inner()).clone();
                config_with_saved_keys(&base, &key_store)
            };
            let effective = match effective {
                Ok(config) => config,
                Err(error) => {
                    error!(
                        provider = %provider_name,
                        reason,
                        rebuild_seq,
                        error = %error,
                        "provider registry async rebuild aborted because config loading failed"
                    );
                    return;
                },
            };

            let new_registry = ProviderRegistry::discover(&effective, &env_overrides).await;

            let current_seq = latest_seq.load(Ordering::Acquire);
            if rebuild_seq != current_seq {
                info!(
                    provider = %provider_name,
                    reason,
                    rebuild_seq,
                    latest_seq = current_seq,
                    elapsed_ms = started.elapsed().as_millis(),
                    "provider registry async rebuild skipped as stale"
                );
                return;
            }

            let provider_summary = new_registry.provider_summary();
            let model_count = new_registry.list_models().len();
            let mut reg = registry.write().await;
            *reg = new_registry;
            info!(
                provider = %provider_name,
                reason,
                rebuild_seq,
                provider_summary = %provider_summary,
                models = model_count,
                elapsed_ms = started.elapsed().as_millis(),
                "provider registry async rebuild finished"
            );
        });
    }

    pub(crate) fn config_snapshot(&self) -> ProvidersConfig {
        self.config
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub(crate) fn set_provider_enabled(&self, provider: &str, enabled: bool) -> ServiceResult<()> {
        if self.config_persistence == ProviderConfigPersistence::Filesystem {
            set_provider_enabled_in_config(provider, enabled)?;
        }
        let mut cfg = self.config.lock().unwrap_or_else(|e| e.into_inner());
        cfg.providers
            .entry(provider.to_string())
            .or_default()
            .enabled = enabled;
        Ok(())
    }

    pub(crate) fn is_provider_configured(
        &self,
        provider: &KnownProvider,
        active_config: &ProvidersConfig,
    ) -> bool {
        if !active_config.is_enabled(provider.name) {
            return false;
        }

        if env_value_with_overrides(&self.env_overrides, provider.env_key).is_some() {
            return true;
        }
        if chelix_config::generic_provider_api_key_from_env(provider.name, &self.env_overrides)
            .is_some()
        {
            return true;
        }
        // Check config file
        if let Some(entry) = active_config.get(provider.name)
            && entry
                .api_key
                .as_ref()
                .is_some_and(|k| !k.expose_secret().is_empty())
        {
            return true;
        }
        // Check persisted key store
        if self.key_store.load(provider.name).is_some() {
            return true;
        }
        // Check persisted key store in user-global config dir.
        if home_key_store()
            .as_ref()
            .is_some_and(|(store, _)| store.load(provider.name).is_some())
        {
            return true;
        }
        false
    }

    /// Build a ProvidersConfig that includes saved keys for registry rebuild.
    pub(crate) fn effective_config(&self) -> ServiceResult<ProvidersConfig> {
        let base = self.config_snapshot();
        config_with_saved_keys(&base, &self.key_store)
    }

    pub(crate) async fn build_registry(&self, config: &ProvidersConfig) -> ProviderRegistry {
        ProviderRegistry::discover(config, &self.env_overrides).await
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

    async fn validate_key(&self, params: Value) -> ServiceResult {
        self.validate_key_inner(params).await
    }

    async fn save_models(&self, params: Value) -> ServiceResult {
        self.save_models_inner(params).await
    }

    async fn add_custom(&self, params: Value) -> ServiceResult {
        self.add_custom_inner(params).await
    }
}
