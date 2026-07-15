use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use {chelix_config::schema::ModelConfigMap, serde_json::Value, tracing::warn};

pub(crate) fn parse_models_param(
    params: &Value,
) -> Result<Option<ModelConfigMap>, serde_json::Error> {
    params
        .get("models")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
}

// ── ProviderConfig ─────────────────────────────────────────────────────────

/// Per-provider stored configuration (API key, base URL, preferred models).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "ModelConfigMap::is_empty")]
    pub models: ModelConfigMap,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

// ── KeyStore ───────────────────────────────────────────────────────────────

/// File-based provider config storage at `~/.config/chelix/provider_keys.json`.
/// Stores per-provider configuration including API keys, base URLs, and models.
#[derive(Debug, Clone)]
pub struct KeyStore {
    inner: Arc<Mutex<KeyStoreInner>>,
}

#[derive(Debug)]
struct KeyStoreInner {
    path: PathBuf,
}

impl Default for KeyStore {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyStore {
    pub fn new() -> Self {
        let path = chelix_config::config_dir()
            .unwrap_or_else(|| PathBuf::from(".config/chelix"))
            .join("provider_keys.json");
        Self {
            inner: Arc::new(Mutex::new(KeyStoreInner { path })),
        }
    }

    pub(crate) fn with_path(path: PathBuf) -> Self {
        Self {
            inner: Arc::new(Mutex::new(KeyStoreInner { path })),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, KeyStoreInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn path(&self) -> PathBuf {
        self.lock().path.clone()
    }

    /// Load all provider configs from the canonical object format.
    fn load_all_configs_from_path(path: &PathBuf) -> HashMap<String, ProviderConfig> {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(error) => {
                if error.kind() != std::io::ErrorKind::NotFound {
                    warn!(
                        path = %path.display(),
                        error = %error,
                        "failed to read provider key store"
                    );
                }
                return HashMap::new();
            },
        };

        serde_json::from_str::<HashMap<String, ProviderConfig>>(&content).unwrap_or_else(|error| {
            warn!(
                path = %path.display(),
                error = %error,
                "provider key store does not match the canonical schema and will be ignored"
            );
            HashMap::new()
        })
    }

    pub fn load_all_configs(&self) -> HashMap<String, ProviderConfig> {
        let guard = self.lock();
        Self::load_all_configs_from_path(&guard.path)
    }

    /// Save all provider configs to disk.
    fn save_all_configs_to_path(
        path: &PathBuf,
        configs: &HashMap<String, ProviderConfig>,
    ) -> crate::error::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                warn!(
                    path = %parent.display(),
                    error = %error,
                    "failed to create provider key store directory"
                );
                crate::error::Error::external(
                    "failed to create provider key store directory",
                    error,
                )
            })?;
        }
        let data = serde_json::to_string_pretty(configs).map_err(|error| {
            warn!(error = %error, "failed to serialize provider key store");
            error
        })?;

        // Write atomically via temp file + rename so readers never observe
        // partially-written JSON.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let temp_path = path.with_extension(format!("json.tmp.{nanos}"));
        std::fs::write(&temp_path, &data).map_err(|error| {
            warn!(
                path = %temp_path.display(),
                error = %error,
                "failed to write provider key store temp file"
            );
            crate::error::Error::external("failed to write provider key store temp file", error)
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&temp_path, std::fs::Permissions::from_mode(0o600));
        }

        std::fs::rename(&temp_path, path).map_err(|error| {
            warn!(
                temp_path = %temp_path.display(),
                path = %path.display(),
                error = %error,
                "failed to atomically replace provider key store"
            );
            crate::error::Error::external("failed to atomically replace provider key store", error)
        })?;

        Ok(())
    }

    /// Load all API keys (used in tests).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn load_all(&self) -> HashMap<String, String> {
        self.load_all_configs()
            .into_iter()
            .filter_map(|(k, v)| v.api_key.map(|key| (k, key)))
            .collect()
    }

    /// Load a provider's API key.
    pub fn load(&self, provider: &str) -> Option<String> {
        self.load_all_configs()
            .get(provider)
            .and_then(|c| c.api_key.clone())
    }

    /// Load a provider's full config.
    pub fn load_config(&self, provider: &str) -> Option<ProviderConfig> {
        self.load_all_configs().get(provider).cloned()
    }

    /// Remove a provider's configuration.
    pub fn remove(&self, provider: &str) -> crate::error::Result<()> {
        let guard = self.lock();
        let mut configs = Self::load_all_configs_from_path(&guard.path);
        configs.remove(provider);
        Self::save_all_configs_to_path(&guard.path, &configs)
    }

    /// Save a provider's API key (simple interface, used in tests).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn save(&self, provider: &str, api_key: &str) -> crate::error::Result<()> {
        self.save_config(
            provider,
            Some(api_key.to_string()),
            None, // preserve existing base_url
            None, // preserve existing models
        )
    }

    /// Save a provider's full configuration.
    pub fn save_config(
        &self,
        provider: &str,
        api_key: Option<String>,
        base_url: Option<String>,
        models: Option<ModelConfigMap>,
    ) -> crate::error::Result<()> {
        self.save_config_with_display_name(provider, api_key, base_url, models, None)
    }

    /// Load all provider configs from vault-encrypted storage, falling back to
    /// plaintext when the vault is unavailable or when the plaintext file is
    /// newer than the encrypted copy (indicating a sync write occurred since
    /// the last vault-unseal encryption).
    #[cfg(feature = "vault")]
    pub async fn load_all_configs_encrypted<C: chelix_vault::Cipher>(
        &self,
        vault: Option<&chelix_vault::Vault<C>>,
    ) -> HashMap<String, ProviderConfig> {
        let path = self.path();

        // If the plaintext is newer than the .enc file, a sync write happened
        // after the last vault-unseal encryption.  Prefer the fresher plaintext
        // so we don't silently return stale data.
        let enc_path = path.with_extension("json.enc");
        if path.exists() && enc_path.exists() {
            let json_mod = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
            let enc_mod = std::fs::metadata(&enc_path).and_then(|m| m.modified()).ok();
            if let (Some(j), Some(e)) = (json_mod, enc_mod)
                && j > e
            {
                return Self::load_all_configs_from_path(&path);
            }
        }

        match chelix_vault::migration::load_encrypted_or_plaintext(vault, &path, "provider_keys")
            .await
        {
            Ok(Some(content)) => serde_json::from_str::<HashMap<String, ProviderConfig>>(&content)
                .unwrap_or_else(|error| {
                    warn!(
                        error = %error,
                        "encrypted provider key store does not match the canonical schema"
                    );
                    HashMap::new()
                }),
            Ok(None) => HashMap::new(),
            Err(chelix_vault::VaultError::Sealed) => {
                warn!("vault sealed, falling back to plaintext provider key store");
                Self::load_all_configs_from_path(&path)
            },
            Err(e) => {
                warn!(error = %e, "failed to decrypt provider key store, falling back to plaintext");
                Self::load_all_configs_from_path(&path)
            },
        }
    }

    /// Save all provider configs with vault encryption when available,
    /// falling back to plaintext.
    ///
    /// Always writes the plaintext `.json` too so sync callers continue to
    /// work until the full async migration is complete.
    #[cfg(feature = "vault")]
    pub async fn save_all_configs_encrypted<C: chelix_vault::Cipher>(
        &self,
        vault: Option<&chelix_vault::Vault<C>>,
        configs: &HashMap<String, ProviderConfig>,
    ) -> crate::error::Result<()> {
        let path = self.path();
        // Always write the plaintext file for sync consumers.
        Self::save_all_configs_to_path(&path, configs)?;

        // Write encrypted copy when vault is available.
        if let Some(vault) = vault {
            let data = serde_json::to_string_pretty(configs).map_err(|error| {
                warn!(error = %error, "failed to serialize provider key store");
                error
            })?;
            if let Err(e) = chelix_vault::migration::save_encrypted_or_plaintext(
                Some(vault),
                &path,
                "provider_keys",
                &data,
            )
            .await
            {
                warn!(error = %e, "failed to write encrypted provider key store");
            }
        }
        Ok(())
    }

    /// Save a provider's full configuration, including an optional display name.
    pub(crate) fn save_config_with_display_name(
        &self,
        provider: &str,
        api_key: Option<String>,
        base_url: Option<String>,
        models: Option<ModelConfigMap>,
        display_name: Option<String>,
    ) -> crate::error::Result<()> {
        let guard = self.lock();
        let mut configs = Self::load_all_configs_from_path(&guard.path);
        let entry = configs.entry(provider.to_string()).or_default();

        // Only update fields that are provided (Some), preserve existing for None
        if let Some(key) = api_key {
            entry.api_key = Some(key);
        }
        if let Some(url) = base_url {
            entry.base_url = if url.is_empty() {
                None
            } else {
                Some(url)
            };
        }
        if let Some(models) = models {
            entry.models = models;
        }
        if let Some(name) = display_name {
            entry.display_name = Some(name);
        }

        Self::save_all_configs_to_path(&guard.path, &configs)
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use {
        super::*,
        chelix_config::schema::{PartialModelMetadata, PartialReasoningMetadata},
    };

    fn model_metadata() -> PartialModelMetadata {
        PartialModelMetadata {
            context_length: Some(128_000),
            max_input_tokens: Some(96_000),
            max_output_tokens: Some(32_000),
            reasoning: Some(PartialReasoningMetadata {
                supported_efforts: Some(Vec::new()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn model_map(ids: &[&str]) -> ModelConfigMap {
        ids.iter()
            .map(|id| ((*id).to_string(), model_metadata()))
            .collect()
    }

    fn model_ids(models: &ModelConfigMap) -> Vec<&str> {
        models.keys().map(String::as_str).collect()
    }

    #[test]
    fn key_store_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyStore::with_path(dir.path().join("keys.json"));
        assert!(store.load("anthropic").is_none());
        store.save("anthropic", "sk-test-123").unwrap();
        assert_eq!(store.load("anthropic").unwrap(), "sk-test-123");
        // Overwrite
        store.save("anthropic", "sk-new").unwrap();
        assert_eq!(store.load("anthropic").unwrap(), "sk-new");
        // Multiple providers
        store.save("openai", "sk-openai").unwrap();
        assert_eq!(store.load("openai").unwrap(), "sk-openai");
        assert_eq!(store.load("anthropic").unwrap(), "sk-new");
        let all = store.load_all();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn key_store_path_reports_backing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.json");
        let store = KeyStore::with_path(path.clone());
        assert_eq!(store.path(), path);
    }

    #[test]
    fn key_store_invalid_json_returns_empty_map() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.json");
        std::fs::write(&path, "{ invalid json").unwrap();

        let store = KeyStore::with_path(path);
        assert!(store.load_all_configs().is_empty());
    }

    #[test]
    fn key_store_remove() {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyStore::with_path(dir.path().join("keys.json"));
        store.save("anthropic", "sk-test").unwrap();
        store.save("openai", "sk-openai").unwrap();
        assert!(store.load("anthropic").is_some());
        store.remove("anthropic").unwrap();
        assert!(store.load("anthropic").is_none());
        // Other keys unaffected
        assert_eq!(store.load("openai").unwrap(), "sk-openai");
        // Removing non-existent key is fine
        store.remove("nonexistent").unwrap();
    }

    #[test]
    fn key_store_save_config_with_all_fields() {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyStore::with_path(dir.path().join("keys.json"));

        // Save full config
        store
            .save_config(
                "openai",
                Some("sk-openai".into()),
                Some("https://custom.api.com/v1".into()),
                Some(model_map(&["gpt-4o", "gpt-4o-mini"])),
            )
            .unwrap();

        let config = store.load_config("openai").unwrap();
        assert_eq!(config.api_key.as_deref(), Some("sk-openai"));
        assert_eq!(
            config.base_url.as_deref(),
            Some("https://custom.api.com/v1")
        );
        assert_eq!(model_ids(&config.models), vec!["gpt-4o", "gpt-4o-mini"]);
        assert!(
            config
                .models
                .values()
                .all(|metadata| metadata.clone().resolve().is_ok())
        );
    }

    #[test]
    fn key_store_save_config_preserves_existing_fields() {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyStore::with_path(dir.path().join("keys.json"));

        // Save initial config with all fields
        store
            .save_config(
                "openai",
                Some("sk-openai".into()),
                Some("https://custom.api.com/v1".into()),
                Some(model_map(&["gpt-4o"])),
            )
            .unwrap();

        // Update only models, preserve others
        store
            .save_config("openai", None, None, Some(model_map(&["gpt-4o-mini"])))
            .unwrap();

        let config = store.load_config("openai").unwrap();
        assert_eq!(config.api_key.as_deref(), Some("sk-openai")); // preserved
        assert_eq!(
            config.base_url.as_deref(),
            Some("https://custom.api.com/v1")
        ); // preserved
        assert_eq!(model_ids(&config.models), vec!["gpt-4o-mini"]); // updated
    }

    #[test]
    fn key_store_save_config_preserves_other_providers() {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyStore::with_path(dir.path().join("keys.json"));

        store
            .save_config(
                "anthropic",
                Some("sk-anthropic".into()),
                Some("https://api.anthropic.com".into()),
                Some(model_map(&["claude-sonnet-4"])),
            )
            .unwrap();

        store
            .save_config(
                "openai",
                Some("sk-openai".into()),
                Some("https://api.openai.com/v1".into()),
                Some(model_map(&["gpt-4o"])),
            )
            .unwrap();

        // Update only OpenAI models, Anthropic should remain unchanged.
        store
            .save_config("openai", None, None, Some(model_map(&["gpt-5"])))
            .unwrap();

        let anthropic = store.load_config("anthropic").unwrap();
        assert_eq!(anthropic.api_key.as_deref(), Some("sk-anthropic"));
        assert_eq!(
            anthropic.base_url.as_deref(),
            Some("https://api.anthropic.com")
        );
        assert_eq!(model_ids(&anthropic.models), vec!["claude-sonnet-4"]);

        let openai = store.load_config("openai").unwrap();
        assert_eq!(openai.api_key.as_deref(), Some("sk-openai"));
        assert_eq!(
            openai.base_url.as_deref(),
            Some("https://api.openai.com/v1")
        );
        assert_eq!(model_ids(&openai.models), vec!["gpt-5"]);
    }

    #[test]
    fn key_store_concurrent_writes_do_not_drop_provider_entries() {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyStore::with_path(dir.path().join("keys.json"));

        let mut handles = Vec::new();
        for (provider, key, models) in [
            ("openai", "sk-openai", model_map(&["gpt-5"])),
            ("anthropic", "sk-anthropic", model_map(&["claude-sonnet-4"])),
        ] {
            let store = store.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..100 {
                    store
                        .save_config(provider, Some(key.to_string()), None, Some(models.clone()))
                        .unwrap();
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        let all = store.load_all_configs();
        assert!(all.contains_key("openai"));
        assert!(all.contains_key("anthropic"));
    }

    #[test]
    fn key_store_save_config_clears_empty_values() {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyStore::with_path(dir.path().join("keys.json"));

        // Save initial config
        store
            .save_config(
                "openai",
                Some("sk-openai".into()),
                Some("https://custom.api.com/v1".into()),
                Some(model_map(&["gpt-4o"])),
            )
            .unwrap();

        // Clear base_url by setting empty string
        store
            .save_config("openai", None, Some(String::new()), None)
            .unwrap();

        let config = store.load_config("openai").unwrap();
        assert_eq!(config.api_key.as_deref(), Some("sk-openai")); // preserved
        assert!(config.base_url.is_none()); // cleared
        assert_eq!(model_ids(&config.models), vec!["gpt-4o"]); // preserved
    }

    #[test]
    fn key_store_rejects_legacy_string_format() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.json");

        let old_data = serde_json::json!({
            "anthropic": "sk-old-key",
            "openai": "sk-openai-old"
        });
        std::fs::write(&path, serde_json::to_string(&old_data).unwrap()).unwrap();

        let store = KeyStore::with_path(path);
        assert!(store.load_all_configs().is_empty());
        assert!(store.load("openai").is_none());
    }

    #[test]
    fn key_store_save_config_with_display_name() {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyStore::with_path(dir.path().join("keys.json"));

        store
            .save_config_with_display_name(
                "custom-together-ai",
                Some("sk-test".into()),
                Some("https://api.together.ai/v1".into()),
                Some(model_map(&["meta-llama/Llama-3-70b"])),
                Some("together.ai".into()),
            )
            .unwrap();

        let config = store.load_config("custom-together-ai").unwrap();
        assert_eq!(config.api_key.as_deref(), Some("sk-test"));
        assert_eq!(
            config.base_url.as_deref(),
            Some("https://api.together.ai/v1")
        );
        assert_eq!(config.display_name.as_deref(), Some("together.ai"));
        assert_eq!(model_ids(&config.models), vec!["meta-llama/Llama-3-70b"]);
    }

    #[test]
    fn models_param_requires_canonical_object_and_preserves_order() {
        let models = model_map(&["gpt-5.2", "anthropic/claude-sonnet-4-5"]);
        let params = serde_json::json!({ "models": models });
        let parsed = parse_models_param(&params).unwrap().unwrap();
        assert_eq!(model_ids(&parsed), vec![
            "gpt-5.2",
            "anthropic/claude-sonnet-4-5"
        ]);

        let legacy = serde_json::json!({ "models": ["gpt-5.2"] });
        assert!(parse_models_param(&legacy).is_err());
    }
}
