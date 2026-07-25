#[cfg(feature = "vault")]
use std::sync::Arc;

#[cfg(feature = "vault")]
use chelix_vault::Vault;

#[cfg(feature = "vault")]
use crate::Error;
use crate::{
    Result,
    credential_store::{CredentialStore, EnvVarEntry, EnvVarValue},
};

impl CredentialStore {
    /// List all environment variables, exposing values only for non-secret entries.
    pub async fn list_env_vars(&self) -> Result<Vec<EnvVarEntry>> {
        let rows: Vec<(i64, String, String, i64, i64, i64, String, String)> = sqlx::query_as(
            "SELECT id, key, value, encrypted, secret, enabled, strftime('%Y-%m-%dT%H:%M:%SZ', created_at), strftime('%Y-%m-%dT%H:%M:%SZ', updated_at) FROM env_variables ORDER BY key ASC",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut entries = Vec::with_capacity(rows.len());
        for (id, key, value, encrypted, secret, enabled, created_at, updated_at) in rows {
            let secret = secret != 0;
            let value = if secret {
                None
            } else {
                Some(self.decrypt_env_value(&key, value, encrypted != 0).await?)
            };
            entries.push(EnvVarEntry {
                id,
                key,
                value,
                secret,
                enabled: enabled != 0,
                created_at,
                updated_at,
                encrypted: encrypted != 0,
            });
        }
        Ok(entries)
    }

    /// Set (upsert) an environment variable.
    ///
    /// When the vault feature is enabled and the vault is unsealed, the value is encrypted before storage.
    pub async fn set_env_var(
        &self,
        key: &str,
        value: &str,
        secret: bool,
        enabled: bool,
    ) -> Result<i64> {
        #[cfg(feature = "vault")]
        let (store_value, encrypted) = {
            if self.is_vault_encryption_enabled() {
                if let Some(ref vault) = self.vault
                    && vault.is_unsealed().await
                {
                    let aad = format!("env:{key}");
                    let enc = vault
                        .encrypt_string(value, &aad)
                        .await
                        .map_err(|e| Error::Crypto(e.to_string()))?;
                    (enc, 1_i64)
                } else {
                    (value.to_owned(), 0_i64)
                }
            } else {
                (value.to_owned(), 0_i64)
            }
        };
        #[cfg(not(feature = "vault"))]
        let (store_value, encrypted) = (value.to_owned(), 0_i64);

        let result = sqlx::query(
            "INSERT INTO env_variables (key, value, encrypted, secret, enabled) VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, encrypted = excluded.encrypted, secret = excluded.secret, enabled = excluded.enabled, updated_at = datetime('now')",
        )
        .bind(key)
        .bind(&store_value)
        .bind(encrypted)
        .bind(secret)
        .bind(enabled)
        .execute(&self.pool)
        .await?;
        Ok(result.last_insert_rowid())
    }

    /// Update visibility and runtime participation without rewriting the value.
    pub async fn update_env_var_flags(&self, id: i64, secret: bool, enabled: bool) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE env_variables SET secret = ?, enabled = ?, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(secret)
        .bind(enabled)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Delete an environment variable by id. Returns the key name if found.
    pub async fn delete_env_var(&self, id: i64) -> Result<Option<String>> {
        let key: Option<(String,)> = sqlx::query_as("SELECT key FROM env_variables WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        sqlx::query("DELETE FROM env_variables WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(key.map(|(k,)| k))
    }

    /// Get decrypted environment variables that are enabled for runtime use.
    pub async fn get_enabled_env_values(&self) -> Result<Vec<EnvVarValue>> {
        let rows: Vec<(String, String, i64, i64)> = sqlx::query_as(
            "SELECT key, value, encrypted, secret FROM env_variables WHERE enabled = 1 ORDER BY key ASC",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut result = Vec::with_capacity(rows.len());
        for (key, value, encrypted, secret) in rows {
            let value = self.decrypt_env_value(&key, value, encrypted != 0).await?;
            result.push(EnvVarValue {
                key,
                value,
                secret: secret != 0,
            });
        }
        Ok(result)
    }

    async fn decrypt_env_value(&self, key: &str, value: String, encrypted: bool) -> Result<String> {
        if !encrypted {
            return Ok(value);
        }

        #[cfg(feature = "vault")]
        {
            let vault = self.vault.as_ref().ok_or_else(|| {
                Error::Crypto(format!(
                    "encrypted environment variable '{key}' requires the vault"
                ))
            })?;
            return vault
                .decrypt_string(&value, &format!("env:{key}"))
                .await
                .map_err(|error| Error::Crypto(error.to_string()));
        }

        #[cfg(not(feature = "vault"))]
        Err(crate::Error::Crypto(format!(
            "encrypted environment variable '{key}' requires vault support"
        )))
    }

    #[cfg(feature = "vault")]
    pub fn vault_for_env(&self) -> Option<&Arc<Vault>> {
        self.vault.as_ref()
    }

    pub async fn audit_log(&self, event_type: &str, client_ip: Option<&str>, detail: Option<&str>) {
        let result = sqlx::query(
            "INSERT INTO auth_audit_log (event_type, client_ip, detail) VALUES (?, ?, ?)",
        )
        .bind(event_type)
        .bind(client_ip)
        .bind(detail)
        .execute(&self.pool)
        .await;
        if let Err(e) = result {
            tracing::debug!(error = %e, "failed to write audit log");
        }

        let _ = sqlx::query(
            "DELETE FROM auth_audit_log WHERE created_at < datetime('now', '-90 days')",
        )
        .execute(&self.pool)
        .await;
    }
}
