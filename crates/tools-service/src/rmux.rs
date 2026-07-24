use std::path::Path;

use anyhow::{Context, Result, bail};

/// Verifies the only host prerequisite required by the in-process terminal runtime.
pub(crate) async fn verify_runtime(default_working_dir: &Path) -> Result<()> {
    let metadata = tokio::fs::metadata(default_working_dir)
        .await
        .with_context(|| {
            format!(
                "tools service working directory is unavailable: {}",
                default_working_dir.display()
            )
        })?;
    if !metadata.is_dir() {
        bail!(
            "tools service working directory is not a directory: {}",
            default_working_dir.display()
        );
    }
    Ok(())
}
