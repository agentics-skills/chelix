use async_trait::async_trait;

use crate::error::Result;

/// Resolves the persisted sandbox owner for a session.
#[async_trait]
pub trait SandboxOwnerResolver: Send + Sync {
    async fn resolve_owner_key(&self, session_key: &str) -> Result<String>;
}

#[cfg(test)]
pub(crate) struct PassthroughSandboxOwnerResolver;

#[cfg(test)]
#[async_trait]
impl SandboxOwnerResolver for PassthroughSandboxOwnerResolver {
    async fn resolve_owner_key(&self, session_key: &str) -> Result<String> {
        Ok(session_key.to_string())
    }
}
