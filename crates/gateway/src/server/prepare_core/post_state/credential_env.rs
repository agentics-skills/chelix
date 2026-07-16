use std::sync::Arc;

use {
    async_trait::async_trait,
    secrecy::{ExposeSecret, Secret},
    tracing::{info, warn},
};

use crate::auth;

pub(super) struct CredentialEnvVarProvider {
    pub(super) store: Arc<auth::CredentialStore>,
    pub(super) gateway_url: Option<String>,
    pub(super) sandbox_api_key: Option<Secret<String>>,
}

#[async_trait]
impl chelix_tools::command::EnvVarProvider for CredentialEnvVarProvider {
    async fn get_env_vars(&self) -> anyhow::Result<Vec<chelix_tools::command::InjectedEnvVar>> {
        let mut vars = self
            .store
            .get_enabled_env_values()
            .await?
            .into_iter()
            .filter(|var| !var.key.starts_with("__CHELIX_"))
            .map(|var| chelix_tools::command::InjectedEnvVar {
                key: var.key,
                value: Secret::new(var.value),
                secret: var.secret,
            })
            .collect::<Vec<_>>();

        if let Some(ref url) = self.gateway_url {
            vars.push(chelix_tools::command::InjectedEnvVar {
                key: "CHELIX_GATEWAY_URL".into(),
                value: Secret::new(url.clone()),
                secret: false,
            });
        }
        if let Some(ref key) = self.sandbox_api_key {
            vars.push(chelix_tools::command::InjectedEnvVar {
                key: "CHELIX_API_KEY".into(),
                value: Secret::new(key.expose_secret().clone()),
                secret: true,
            });
        }

        Ok(vars)
    }
}

pub(super) async fn ensure_sandbox_api_key(store: &auth::CredentialStore) -> Option<String> {
    if let Ok(vals) = store.get_enabled_env_values().await
        && let Some(var) = vals
            .iter()
            .find(|var| var.key == "__CHELIX_SANDBOX_API_KEY")
    {
        return Some(var.value.clone());
    }

    let scopes = vec!["operator.read".to_string(), "operator.write".to_string()];
    match store.create_api_key("sandbox-ctl", Some(&scopes)).await {
        Ok((_id, raw_key)) => {
            if let Err(e) = store
                .set_env_var("__CHELIX_SANDBOX_API_KEY", &raw_key, true, true)
                .await
            {
                warn!(error = %e, "failed to persist sandbox API key");
            }
            info!("created sandbox-ctl API key for chelix-ctl");
            Some(raw_key)
        },
        Err(e) => {
            warn!(error = %e, "failed to create sandbox API key");
            None
        },
    }
}
