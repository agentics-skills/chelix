//! Generic webhook ingress for Chelix.
//!
//! Provides named inbound HTTP endpoints that trigger agent sessions.
//! Each delivery is verified, deduplicated, persisted, and processed
//! asynchronously via the existing chat/session infrastructure.

pub mod auth;
pub mod dedup;
pub mod error;
pub mod filter;
pub mod normalize;
pub mod profiles;
pub mod rate_limit;
pub mod store;
pub mod types;
pub mod worker;

pub use error::{Error, Result};

/// Run database migrations for the webhooks crate.
///
/// Creates the `webhooks`, `webhook_deliveries`, and `webhook_response_actions`
/// tables. Call at application startup.
pub async fn run_migrations(pool: &sqlx::SqlitePool) -> Result<()> {
    // Foreign key enforcement (for ON DELETE CASCADE) is enabled via
    // `.foreign_keys(true)` on the pool's SqliteConnectOptions, which
    // applies to every connection — not per-query PRAGMA.
    sqlx::migrate!("./migrations")
        .set_ignore_missing(true)
        .run(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use sqlx::SqlitePool;

    async fn pre_model_reasoning_pool() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        for migration in [
            include_str!("../migrations/20260407000000_initial.sql"),
            include_str!("../migrations/20260407100000_unique_delivery_key.sql"),
            include_str!("../migrations/20260429000000_deliver_only.sql"),
        ] {
            sqlx::raw_sql(migration).execute(&pool).await.unwrap();
        }
        pool
    }

    async fn insert_webhook(pool: &SqlitePool, public_id: &str, model: Option<&str>) {
        sqlx::query(
            "INSERT INTO webhooks (name, public_id, model, auth_mode, source_profile, \
             session_mode, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind("hook")
        .bind(public_id)
        .bind(model)
        .bind("none")
        .bind("generic")
        .bind("per_delivery")
        .bind("2026-07-30T00:00:00Z")
        .bind("2026-07-30T00:00:00Z")
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn model_reasoning_migration_preserves_rows_without_an_override() {
        let pool = pre_model_reasoning_pool().await;
        insert_webhook(&pool, "wh-valid", None).await;

        sqlx::raw_sql(include_str!(
            "../migrations/20260730120000_model_reasoning_pair.sql"
        ))
        .execute(&pool)
        .await
        .unwrap();

        let stored = sqlx::query_as::<_, (String, Option<String>, Option<String>)>(
            "SELECT public_id, model, reasoning_effort FROM webhooks WHERE public_id = ?",
        )
        .bind("wh-valid")
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(stored, ("wh-valid".into(), None, None));

        sqlx::query(
            "INSERT INTO webhooks (name, public_id, model, reasoning_effort, auth_mode, \
             source_profile, session_mode, created_at, updated_at) \
             VALUES ('valid', 'wh-pair', 'test::model', 'off', 'none', 'generic', \
                     'per_delivery', '2026-07-30T00:00:00Z', '2026-07-30T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        for (model, reasoning_effort) in [
            (Some("test::model"), None),
            (None, Some("off")),
            (Some(""), Some("off")),
            (Some("test::model"), Some("")),
        ] {
            let result = sqlx::query(
                "INSERT INTO webhooks (name, public_id, model, reasoning_effort, auth_mode, \
                 source_profile, session_mode, created_at, updated_at) \
                 VALUES ('invalid', hex(randomblob(16)), ?, ?, 'none', 'generic', \
                         'per_delivery', '2026-07-30T00:00:00Z', '2026-07-30T00:00:00Z')",
            )
            .bind(model)
            .bind(reasoning_effort)
            .execute(&pool)
            .await;
            assert!(result.is_err());
        }
    }

    #[tokio::test]
    async fn model_reasoning_migration_rejects_model_only_state_without_rewrite() {
        let pool = pre_model_reasoning_pool().await;
        insert_webhook(&pool, "wh-invalid", Some("test::model")).await;

        let migration = sqlx::raw_sql(include_str!(
            "../migrations/20260730120000_model_reasoning_pair.sql"
        ))
        .execute(&pool)
        .await;
        assert!(migration.is_err());

        let stored: Option<String> =
            sqlx::query_scalar("SELECT model FROM webhooks WHERE public_id = 'wh-invalid'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(stored.as_deref(), Some("test::model"));
        let reasoning_column_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('webhooks') WHERE name = 'reasoning_effort'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(reasoning_column_count, 0);
    }
}
