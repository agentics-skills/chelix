use std::sync::Arc;

use {async_trait::async_trait, secrecy::Secret};

use crate::auth;

pub(super) struct CredentialEnvVarProvider {
    pub(super) store: Arc<auth::CredentialStore>,
}

#[async_trait]
impl chelix_tools::command::EnvVarProvider for CredentialEnvVarProvider {
    async fn get_env_vars(&self) -> anyhow::Result<Vec<chelix_tools::command::InjectedEnvVar>> {
        Ok(self
            .store
            .get_enabled_env_values()
            .await?
            .into_iter()
            .map(|var| chelix_tools::command::InjectedEnvVar {
                key: var.key,
                value: Secret::new(var.value),
                secret: var.secret,
            })
            .collect())
    }
}
