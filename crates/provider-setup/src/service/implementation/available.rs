//! Provider listing — `available()` implementation.

use std::collections::HashMap;

use serde_json::Value;

use chelix_service_traits::ServiceResult;

use {
    super::LiveProviderSetupService,
    crate::{
        config_helpers::{
            normalize_provider_name, ui_offered_provider_order, ui_offered_provider_set,
        },
        custom_providers::is_custom_provider,
        known_providers::known_providers,
    },
};

impl LiveProviderSetupService {
    pub(super) async fn available_inner(&self) -> ServiceResult {
        let is_cloud = self.deploy_platform.is_some();
        let active_config = self.effective_config();
        let offered_order = ui_offered_provider_order(&active_config);
        let offered = ui_offered_provider_set(&offered_order);
        let offered_rank: HashMap<String, usize> = offered_order
            .iter()
            .enumerate()
            .map(|(idx, provider)| (provider.clone(), idx))
            .collect();

        let mut providers: Vec<(Option<usize>, usize, Value)> = known_providers()
            .iter()
            .enumerate()
            .filter_map(|(known_idx, provider)| {
                // Hide local-only providers on cloud deployments.
                if is_cloud && provider.is_local_only() {
                    return None;
                }

                let configured = self.is_provider_configured(provider, &active_config);
                let normalized_name = normalize_provider_name(provider.name);
                if let Some(allowed) = offered.as_ref()
                    && !allowed.contains(&normalized_name)
                    && !configured
                {
                    return None;
                }

                let entry = active_config.get(provider.name);
                let base_url = entry.and_then(|config| config.base_url.clone());
                let models = entry
                    .map(|config| config.models.clone())
                    .unwrap_or_default();

                Some((
                    offered_rank.get(&normalized_name).copied(),
                    known_idx,
                    serde_json::json!({
                        "name": provider.name,
                        "displayName": provider.display_name,
                        "authType": provider.auth_type.as_str(),
                        "configured": configured,
                        "defaultBaseUrl": provider.default_base_url,
                        "baseUrl": base_url,
                        "models": models,
                        "requiresModel": provider.requires_model,
                        "keyOptional": provider.key_optional,
                    }),
                ))
            })
            .collect();

        // Append custom providers from the key store.
        let known_count = providers.len();
        for (name, config) in self.key_store.load_all_configs() {
            if !is_custom_provider(&name) {
                continue;
            }
            if active_config.get(&name).is_some_and(|entry| !entry.enabled) {
                continue;
            }
            let display_name = config.display_name.clone().unwrap_or_else(|| name.clone());
            let entry = active_config.get(&name);
            let base_url = entry
                .and_then(|provider| provider.base_url.clone())
                .or(config.base_url.clone());
            let models = entry
                .map(|provider| provider.models.clone())
                .unwrap_or_default();

            providers.push((
                None,
                known_count, // sort after all known providers
                serde_json::json!({
                    "name": name,
                    "displayName": display_name,
                    "authType": "api-key",
                    "configured": true,
                    "defaultBaseUrl": base_url,
                    "baseUrl": base_url,
                    "models": models,
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
