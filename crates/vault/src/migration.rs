//! Helpers for migrating plaintext data to encrypted vault storage.
//!
//! On the first vault unseal, plaintext secrets are encrypted in-place:
//! - Env vars: rows with `encrypted = 0` are encrypted and flagged.
//! - Managed SSH keys: rows with `encrypted = 0` are encrypted and flagged.

use crate::{error::VaultError, traits::Cipher, vault::Vault};

/// Encrypt all plaintext env variable rows (where `encrypted = 0`).
///
/// Each value is encrypted with AAD `"env:<key>"` for domain separation.
pub async fn migrate_env_vars<C: Cipher>(
    vault: &Vault<C>,
    pool: &sqlx::SqlitePool,
) -> Result<usize, VaultError> {
    let rows: Vec<(i64, String, String)> =
        sqlx::query_as("SELECT id, key, value FROM env_variables WHERE encrypted = 0")
            .fetch_all(pool)
            .await?;

    let count = rows.len();
    for (id, key, plaintext) in rows {
        let aad = format!("env:{key}");
        let encrypted = vault.encrypt_string(&plaintext, &aad).await?;

        sqlx::query("UPDATE env_variables SET value = ?, encrypted = 1, updated_at = datetime('now') WHERE id = ?")
            .bind(&encrypted)
            .bind(id)
            .execute(pool)
            .await?;
    }

    if count > 0 {
        #[cfg(feature = "tracing")]
        tracing::info!(count, "migrated env variables to encrypted storage");
    }

    Ok(count)
}

/// Encrypt all plaintext managed SSH private keys (where `encrypted = 0`).
///
/// Each private key is encrypted with AAD `"ssh-key:<name>"` for domain
/// separation.
pub async fn migrate_ssh_keys<C: Cipher>(
    vault: &Vault<C>,
    pool: &sqlx::SqlitePool,
) -> Result<usize, VaultError> {
    let rows: Vec<(i64, String, String)> =
        sqlx::query_as("SELECT id, name, private_key FROM ssh_keys WHERE encrypted = 0")
            .fetch_all(pool)
            .await?;

    let count = rows.len();
    for (id, name, plaintext) in rows {
        let aad = format!("ssh-key:{name}");
        let encrypted = vault.encrypt_string(&plaintext, &aad).await?;

        sqlx::query(
            "UPDATE ssh_keys SET private_key = ?, encrypted = 1, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(&encrypted)
        .bind(id)
        .execute(pool)
        .await?;
    }

    if count > 0 {
        #[cfg(feature = "tracing")]
        tracing::info!(count, "migrated ssh keys to encrypted storage");
    }

    Ok(count)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use {super::*, crate::xchacha20::XChaCha20Poly1305Cipher};

    async fn setup_vault() -> (sqlx::SqlitePool, Vault<XChaCha20Poly1305Cipher>) {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS vault_metadata (
                id                   INTEGER PRIMARY KEY CHECK (id = 1),
                version              INTEGER NOT NULL DEFAULT 1,
                kdf_salt             TEXT NOT NULL,
                kdf_params           TEXT NOT NULL,
                wrapped_dek          TEXT NOT NULL,
                recovery_wrapped_dek TEXT,
                recovery_key_hash    TEXT,
                created_at           TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at           TEXT NOT NULL DEFAULT (datetime('now'))
            )",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS env_variables (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                key        TEXT    NOT NULL UNIQUE,
                value      TEXT    NOT NULL,
                encrypted  INTEGER NOT NULL DEFAULT 0,
                created_at TEXT    NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT    NOT NULL DEFAULT (datetime('now'))
            )",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS ssh_keys (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                name        TEXT    NOT NULL UNIQUE,
                private_key TEXT    NOT NULL,
                public_key  TEXT    NOT NULL,
                fingerprint TEXT    NOT NULL,
                encrypted   INTEGER NOT NULL DEFAULT 0,
                created_at  TEXT    NOT NULL DEFAULT (datetime('now')),
                updated_at  TEXT    NOT NULL DEFAULT (datetime('now'))
            )",
        )
        .execute(&pool)
        .await
        .unwrap();

        let vault = Vault::with_cipher(pool.clone(), XChaCha20Poly1305Cipher)
            .await
            .unwrap();
        vault.initialize("testpassword").await.unwrap();

        (pool, vault)
    }

    #[tokio::test]
    async fn migrate_env_vars_encrypts_plaintext() {
        let (pool, vault) = setup_vault().await;

        // Insert plaintext rows.
        sqlx::query("INSERT INTO env_variables (key, value) VALUES ('KEY1', 'secret1')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO env_variables (key, value) VALUES ('KEY2', 'secret2')")
            .execute(&pool)
            .await
            .unwrap();

        let count = migrate_env_vars(&vault, &pool).await.unwrap();
        assert_eq!(count, 2);

        // Verify encrypted flag.
        let rows: Vec<(i64, String, i32)> =
            sqlx::query_as("SELECT id, value, encrypted FROM env_variables ORDER BY key")
                .fetch_all(&pool)
                .await
                .unwrap();
        for (_, value, encrypted) in &rows {
            assert_eq!(*encrypted, 1);
            assert_ne!(value, "secret1");
            assert_ne!(value, "secret2");
        }

        // Verify decryption works.
        let decrypted = vault.decrypt_string(&rows[0].1, "env:KEY1").await.unwrap();
        assert_eq!(decrypted, "secret1");

        // Running again is a no-op.
        let count2 = migrate_env_vars(&vault, &pool).await.unwrap();
        assert_eq!(count2, 0);
    }

    #[tokio::test]
    async fn migrate_ssh_keys_encrypts_plaintext() {
        let (pool, vault) = setup_vault().await;

        sqlx::query(
            "INSERT INTO ssh_keys (name, private_key, public_key, fingerprint)
             VALUES ('prod-box', 'PRIVATE KEY', 'ssh-ed25519 AAAA test', 'SHA256:test')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let count = migrate_ssh_keys(&vault, &pool).await.unwrap();
        assert_eq!(count, 1);

        let row: (String, i32) =
            sqlx::query_as("SELECT private_key, encrypted FROM ssh_keys WHERE name = 'prod-box'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(row.1, 1);
        assert_ne!(row.0, "PRIVATE KEY");

        let decrypted = vault
            .decrypt_string(&row.0, "ssh-key:prod-box")
            .await
            .unwrap();
        assert_eq!(decrypted, "PRIVATE KEY");

        let count2 = migrate_ssh_keys(&vault, &pool).await.unwrap();
        assert_eq!(count2, 0);
    }
}
