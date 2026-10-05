//! Provider listing — `available()` implementation.

use std::collections::HashMap;

use {secrecy::ExposeSecret, serde_json::Value};

use chelix_service_traits::ServiceResult;

use {super::LiveProviderSetupService, crate::config_helpers::ui_offered_provider_order};

impl LiveProviderSetupService {
    pub(super) async fn available_inner(&self) -> ServiceResult {
        let active_config = self.effective_config()?;
        let offered_order = ui_offered_provider_order(&active_config);
        let offered_rank: HashMap<String, usize> = offered_order
            .iter()
            .enumerate()
            .map(|(idx, provider)| (provider.clone(), idx))
            .collect();

        let mut providers: Vec<(Option<usize>, Value)> = Vec::new();
        for (name, entry) in &active_config.providers {
            let display_name = name.clone();
            let base_url = entry.base_url.clone();
            let alias = entry.alias.clone();
            let configured = entry
                .api_key
                .as_ref()
                .is_some_and(|api_key| !api_key.expose_secret().is_empty());
            let normalized_name = chelix_config::normalize_provider_name(name).unwrap_or_default();

            providers.push((
                offered_rank.get(&normalized_name).copied(),
                serde_json::json!({
                    "name": name,
                    "displayName": display_name,
                    "configured": configured,
                    "defaultBaseUrl": base_url,
                    "baseUrl": base_url,
                    "alias": alias,
                    "requiresModel": true,
                    "keyOptional": false,
                    "isOpenAiCompatible": true,
                    "enabled": entry.enabled,
                    "wireApi": match entry.wire_api {
                        chelix_config::schema::WireApi::ChatCompletions => "chat-completions",
                        chelix_config::schema::WireApi::Responses => "responses",
                    },
                    "toolMode": match entry.tool_mode {
                        chelix_config::schema::ToolMode::Native => "native",
                        chelix_config::schema::ToolMode::Text => "text",
                        chelix_config::schema::ToolMode::Off => "off",
                    },
                }),
            ));
        }

        providers.sort_by(|(a_offered, a_value), (b_offered, b_value)| {
            let offered_cmp = match (a_offered, b_offered) {
                (Some(a), Some(b)) => a.cmp(b),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            };
            if offered_cmp != std::cmp::Ordering::Equal {
                return offered_cmp;
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
        });

        let providers: Vec<Value> = providers
            .into_iter()
            .enumerate()
            .map(|(idx, (_, mut value))| {
                if let Some(obj) = value.as_object_mut() {
                    obj.insert("uiOrder".into(), serde_json::json!(idx));
                }
                value
            })
            .collect();

        Ok(Value::Array(providers))
    }
}
