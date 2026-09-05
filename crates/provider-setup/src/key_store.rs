use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use tracing::warn;

// ── ProviderConfig ─────────────────────────────────────────────────────────

/// Per-provider stored credentials and endpoint configuration.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

// ── KeyStore ───────────────────────────────────────────────────────────────

/// File-based provider credential storage at `~/.config/chelix/provider_keys.json`.
/// Stores API keys, base URLs, and custom provider display names.
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

    #[cfg(test)]
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
    fn load_all_configs_from_path(
        path: &PathBuf,
    ) -> crate::error::Result<HashMap<String, ProviderConfig>> {
        let content = match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(HashMap::new());
            },
            Err(error) => {
                warn!(
                    path = %path.display(),
                    error = %error,
                    "failed to read provider key store"
                );
                return Err(crate::error::Error::external(
                    "failed to read provider key store",
                    error,
                ));
            },
        };

        serde_json::from_str::<HashMap<String, ProviderConfig>>(&content).map_err(|error| {
            warn!(
                path = %path.display(),
                error = %error,
                "provider key store does not match the canonical schema"
            );
            crate::error::Error::external("failed to parse provider key store", error)
        })
    }

    pub fn load_all_configs(&self) -> crate::error::Result<HashMap<String, ProviderConfig>> {
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
    pub(crate) fn load_all(&self) -> crate::error::Result<HashMap<String, String>> {
        Ok(self
            .load_all_configs()?
            .into_iter()
            .filter_map(|(key, config)| config.api_key.map(|api_key| (key, api_key)))
            .collect())
    }

    /// Load a provider's API key.
    pub fn load(&self, provider: &str) -> crate::error::Result<Option<String>> {
        Ok(self
            .load_all_configs()?
            .get(provider)
            .and_then(|config| config.api_key.clone()))
    }

    /// Load a provider's full config.
    pub fn load_config(&self, provider: &str) -> crate::error::Result<Option<ProviderConfig>> {
        Ok(self.load_all_configs()?.get(provider).cloned())
    }

    /// Remove a provider's configuration.
    pub fn remove(&self, provider: &str) -> crate::error::Result<()> {
        let guard = self.lock();
        let mut configs = Self::load_all_configs_from_path(&guard.path)?;
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
        )
    }

    /// Save a provider's full configuration.
    pub fn save_config(
        &self,
        provider: &str,
        api_key: Option<String>,
        base_url: Option<String>,
    ) -> crate::error::Result<()> {
        self.save_config_with_display_name(provider, api_key, base_url, None)
    }

    /// Load all provider configs from the newest canonical store copy.
    ///
    /// A newer plaintext copy is read directly. Otherwise, an existing encrypted
    /// copy must be decrypted successfully; read, decrypt, and schema errors are
    /// propagated. Plaintext is used when no encrypted copy exists.
    #[cfg(feature = "vault")]
    pub async fn load_all_configs_encrypted<C: chelix_vault::Cipher>(
        &self,
        vault: Option<&chelix_vault::Vault<C>>,
    ) -> crate::error::Result<HashMap<String, ProviderConfig>> {
        let path = self.path();

        // If the plaintext is newer than the encrypted file, a synchronous write
        // happened after encryption. Read the newer canonical file.
        let enc_path = path.with_extension("json.enc");
        if path.exists() && enc_path.exists() {
            let json_mod = std::fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .ok();
            let enc_mod = std::fs::metadata(&enc_path)
                .and_then(|metadata| metadata.modified())
                .ok();
            if let (Some(json_modified), Some(encrypted_modified)) = (json_mod, enc_mod)
                && json_modified > encrypted_modified
            {
                return Self::load_all_configs_from_path(&path);
            }
        }

        match chelix_vault::migration::load_encrypted_or_plaintext(vault, &path, "provider_keys")
            .await
        {
            Ok(Some(content)) => serde_json::from_str::<HashMap<String, ProviderConfig>>(&content)
                .map_err(|error| {
                    warn!(
                        error = %error,
                        "encrypted provider key store does not match the canonical schema"
                    );
                    crate::error::Error::external(
                        "failed to parse encrypted provider key store",
                        error,
                    )
                }),
            Ok(None) => Ok(HashMap::new()),
            Err(error) => {
                warn!(error = %error, "failed to load encrypted provider key store");
                Err(crate::error::Error::external(
                    "failed to load encrypted provider key store",
                    error,
                ))
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
        display_name: Option<String>,
    ) -> crate::error::Result<()> {
        let guard = self.lock();
        let mut configs = Self::load_all_configs_from_path(&guard.path)?;
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
        if let Some(name) = display_name {
            entry.display_name = Some(name);
        }

        Self::save_all_configs_to_path(&guard.path, &configs)
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_store_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyStore::with_path(dir.path().join("keys.json"));
        assert!(store.load("openrouter").unwrap().is_none());

        store.save("openrouter", "test-123").unwrap();
        assert_eq!(
            store.load("openrouter").unwrap().as_deref(),
            Some("test-123")
        );

        store.save("openrouter", "new-key").unwrap();
        store.save("openai", "sk-openai").unwrap();
        assert_eq!(
            store.load("openrouter").unwrap().as_deref(),
            Some("new-key")
        );
        assert_eq!(store.load("openai").unwrap().as_deref(), Some("sk-openai"));
        assert_eq!(store.load_all().unwrap().len(), 2);
    }

    #[test]
    fn key_store_path_reports_backing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.json");
        let store = KeyStore::with_path(path.clone());
        assert_eq!(store.path(), path);
    }

    #[test]
    fn key_store_invalid_json_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.json");
        std::fs::write(&path, "{ invalid json").unwrap();

        let error = KeyStore::with_path(path)
            .load_all_configs()
            .expect_err("invalid JSON must be rejected");
        assert!(
            error
                .to_string()
                .contains("failed to parse provider key store")
        );
    }

    #[test]
    fn key_store_rejects_model_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "openai": {
                    "apiKey": "sk-test",
                    "models": {}
                }
            })
            .to_string(),
        )
        .unwrap();

        let error = KeyStore::with_path(path)
            .load_all_configs()
            .expect_err("model metadata must not be accepted by the credential store");
        assert!(
            error
                .to_string()
                .contains("failed to parse provider key store")
        );
        assert!(error.to_string().contains("unknown field `models`"));
    }

    #[test]
    fn key_store_remove_preserves_other_entries() {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyStore::with_path(dir.path().join("keys.json"));
        store.save("openrouter", "test-key").unwrap();
        store.save("openai", "sk-openai").unwrap();

        store.remove("openrouter").unwrap();
        assert!(store.load("openrouter").unwrap().is_none());
        assert_eq!(store.load("openai").unwrap().as_deref(), Some("sk-openai"));

        store.remove("nonexistent").unwrap();
    }

    #[test]
    fn key_store_save_config_preserves_unspecified_fields() {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyStore::with_path(dir.path().join("keys.json"));
        store
            .save_config(
                "openai",
                Some("sk-openai".into()),
                Some("https://custom.api.com/v1".into()),
            )
            .unwrap();

        store
            .save_config("openai", None, Some(String::new()))
            .unwrap();

        let config = store.load_config("openai").unwrap().unwrap();
        assert_eq!(config.api_key.as_deref(), Some("sk-openai"));
        assert!(config.base_url.is_none());
    }

    #[test]
    fn key_store_concurrent_writes_do_not_drop_provider_entries() {
        let dir = tempfile::tempdir().unwrap();
        let store = KeyStore::with_path(dir.path().join("keys.json"));
        let mut handles = Vec::new();

        for (provider, key) in [("openai", "sk-openai"), ("openrouter", "openrouter-key")] {
            let store = store.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..100 {
                    store
                        .save_config(provider, Some(key.to_string()), None)
                        .unwrap();
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        let all = store.load_all_configs().unwrap();
        assert!(all.contains_key("openai"));
        assert!(all.contains_key("openrouter"));
    }

    #[test]
    fn key_store_rejects_legacy_string_format() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "openrouter": "old-key",
                "openai": "sk-openai-old"
            })
            .to_string(),
        )
        .unwrap();

        let error = KeyStore::with_path(path)
            .load_all_configs()
            .expect_err("legacy string entries must be rejected");
        assert!(
            error
                .to_string()
                .contains("failed to parse provider key store")
        );
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
                Some("together.ai".into()),
            )
            .unwrap();

        let config = store.load_config("custom-together-ai").unwrap().unwrap();
        assert_eq!(config.api_key.as_deref(), Some("sk-test"));
        assert_eq!(
            config.base_url.as_deref(),
            Some("https://api.together.ai/v1")
        );
        assert_eq!(config.display_name.as_deref(), Some("together.ai"));
    }
}
