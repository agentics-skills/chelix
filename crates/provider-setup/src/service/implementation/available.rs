//! Provider listing — `available()` implementation.

use std::collections::HashMap;

use {secrecy::ExposeSecret, serde_json::Value};

use chelix_service_traits::{ServiceError, ServiceResult};

use {
    super::LiveProviderSetupService,
    crate::{
        config_helpers::{
            is_custom_provider, normalize_provider_name, ui_offered_provider_order,
            ui_offered_provider_set,
        },
        known_providers::known_providers,
    },
};

impl LiveProviderSetupService {
    pub(super) async fn available_inner(&self) -> ServiceResult {
        let is_cloud = self.deploy_platform.is_some();
        let active_config = self.effective_config()?;
        let offered_order = ui_offered_provider_order(&active_config);
        let offered = ui_offered_provider_set(&offered_order);
        let offered_rank: HashMap<String, usize> = offered_order
            .iter()
            .enumerate()
            .map(|(idx, provider)| (provider.clone(), idx))
            .collect();

        let mut providers: Vec<(Option<usize>, usize, Value)> = Vec::new();
        for (known_idx, provider) in known_providers().iter().enumerate() {
            // Hide local-only providers on cloud deployments.
            if is_cloud && provider.is_local_only() {
                continue;
            }

            let configured = self.is_provider_configured(provider, &active_config)?;
            let normalized_name = normalize_provider_name(provider.name);
            if let Some(allowed) = offered.as_ref()
                && !allowed.contains(&normalized_name)
                && !configured
            {
                continue;
            }

            let entry = active_config.get(provider.name);
            let base_url = entry.and_then(|config| config.base_url.clone());

            providers.push((
                offered_rank.get(&normalized_name).copied(),
                known_idx,
                serde_json::json!({
                    "name": provider.name,
                    "displayName": provider.display_name,
                    "configured": configured,
                    "defaultBaseUrl": provider.default_base_url,
                    "baseUrl": base_url,
                    "requiresModel": provider.requires_model,
                    "keyOptional": provider.key_optional,
                }),
            ));
        }

        // Append OpenAI-compatible providers declared in the service config.
        let saved_configs = self
            .key_store
            .load_all_configs()
            .map_err(ServiceError::message)?;
        let known_count = providers.len();
        for (name, entry) in &active_config.providers {
            if !is_custom_provider(name) {
                continue;
            }
            let saved = saved_configs.get(name);
            let display_name = saved
                .and_then(|config| config.display_name.clone())
                .unwrap_or_else(|| format!("OpenAI Compatible ({name})"));
            let base_url = entry
                .base_url
                .clone()
                .or_else(|| saved.and_then(|config| config.base_url.clone()));
            let configured = entry
                .api_key
                .as_ref()
                .is_some_and(|api_key| !api_key.expose_secret().is_empty());

            providers.push((
                None,
                known_count, // sort after all known providers
                serde_json::json!({
                    "name": name,
                    "displayName": display_name,
                    "configured": configured,
                    "defaultBaseUrl": base_url,
                    "baseUrl": base_url,
                    "requiresModel": true,
                    "keyOptional": false,
                    "isCustom": true,
                }),
            ));
        }

        providers.sort_by(
            |(a_offered, a_known, a_value), (b_offered, b_known, b_value)| {
                let offered_cmp = match (a_offered, b_offered) {
                    (Some(a), Some(b)) => a.cmp(b),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                };
                if offered_cmp != std::cmp::Ordering::Equal {
                    return offered_cmp;
                }

                let known_cmp = a_known.cmp(b_known);
                if known_cmp != std::cmp::Ordering::Equal {
                    return known_cmp;
                }

                let a_name = a_value
                    .get("displayName")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let b_name = b_value
                    .get("displayName")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                a_name.cmp(b_name)
            },
        );

        let providers: Vec<Value> = providers
            .into_iter()
            .enumerate()
            .map(|(idx, (_, _, mut value))| {
                if let Some(obj) = value.as_object_mut() {
                    obj.insert("uiOrder".into(), serde_json::json!(idx));
                }
                value
            })
            .collect();

        Ok(Value::Array(providers))
    }
}
