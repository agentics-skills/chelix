//! Schema migrations for the dedicated semantic history database.

use crate::Result;

/// Run semantic history migrations against the `ui-history.sqlite` pool.
pub async fn run_migrations(pool: &sqlx::SqlitePool) -> Result<()> {
    sqlx::migrate!("./migrations/ui-history").run(pool).await?;
    Ok(())
}
