//! Gateway adapter: wraps `LiveOnboardingService` to implement `OnboardingService`.

use std::sync::Arc;

use {async_trait::async_trait, serde_json::Value};

use crate::services::{OnboardingService, ServiceError, ServiceResult};

/// Gateway-side onboarding service backed by `chelix_onboarding::service::LiveOnboardingService`.
pub struct GatewayOnboardingService {
    inner: chelix_onboarding::service::LiveOnboardingService,
    gateway_state: Arc<tokio::sync::OnceCell<Arc<crate::state::GatewayState>>>,
}

impl GatewayOnboardingService {
    pub fn new(
        inner: chelix_onboarding::service::LiveOnboardingService,
        gateway_state: Arc<tokio::sync::OnceCell<Arc<crate::state::GatewayState>>>,
    ) -> Self {
        Self {
            inner,
            gateway_state,
        }
    }
}

#[async_trait]
impl OnboardingService for GatewayOnboardingService {
    async fn wizard_start(&self, params: Value) -> ServiceResult {
        let force = params
            .get("force")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Ok(self.inner.wizard_start(force))
    }

    async fn wizard_next(&self, params: Value) -> ServiceResult {
        let input = params.get("input").and_then(|v| v.as_str()).unwrap_or("");
        self.inner.wizard_next(input).map_err(ServiceError::message)
    }

    async fn wizard_cancel(&self) -> ServiceResult {
        self.inner.wizard_cancel();
        Ok(serde_json::json!({}))
    }

    async fn wizard_status(&self) -> ServiceResult {
        Ok(self.inner.wizard_status())
    }

    async fn identity_get(&self) -> ServiceResult {
        Ok(serde_json::to_value(self.inner.identity_get()).unwrap_or_default())
    }

    async fn identity_update(&self, params: Value) -> ServiceResult {
        let response = self
            .inner
            .identity_update(params)
            .map_err(ServiceError::message)?;

        if let Some(state) = self.gateway_state.get()
            && let Some(location_value) = response.get("user_location")
        {
            let mut inner = state.inner.write().await;
            if location_value.is_null() {
                inner.cached_location = None;
            } else if let Some(location) = parse_geo_location(location_value) {
                inner.cached_location = Some(location);
            }
        }

        Ok(response)
    }

    async fn identity_update_soul(&self, soul: Option<String>) -> ServiceResult {
        self.inner
            .identity_update_soul(soul)
            .map_err(ServiceError::message)
    }

    // ── Claude import ───────────────────────────────────────────────────────

    #[cfg(feature = "claude-import")]
    async fn claude_detect(&self) -> ServiceResult {
        let detection = chelix_claude_import::detect::detect();
        match detection {
            Some(d) => {
                let skills = chelix_claude_import::skills::discover_skills(&d);
                let commands = chelix_claude_import::skills::discover_commands(&d);
                let has_mcp = d.user_claude_json_path.is_some() || d.desktop_config_path.is_some();

                tracing::info!(
                    has_settings = d.user_settings_path.is_some(),
                    has_mcp,
                    skills = skills.len(),
                    commands = commands.len(),
                    has_memory = d.user_memory_path.is_some(),
                    "claude.detect: installation detected"
                );

                Ok(serde_json::json!({
                    "detected": true,
                    "has_mcp_servers": has_mcp,
                    "has_desktop_config": d.desktop_config_path.is_some(),
                    "skills_count": skills.len(),
                    "commands_count": commands.len(),
                    "has_memory": d.user_memory_path.is_some(),
                }))
            },
            None => {
                tracing::info!("claude.detect: no installation detected");
                Ok(serde_json::json!({ "detected": false }))
            },
        }
    }

    #[cfg(not(feature = "claude-import"))]
    async fn claude_detect(&self) -> ServiceResult {
        Ok(serde_json::json!({ "detected": false }))
    }

    #[cfg(feature = "claude-import")]
    async fn claude_import(&self, params: Value) -> ServiceResult {
        let detection = chelix_claude_import::detect::detect()
            .ok_or_else(|| "no Claude Code installation found".to_string())?;

        let data_dir = chelix_config::data_dir();
        let mcp_path = data_dir.join("mcp-servers.json");
        let skills_dir = data_dir.join("skills");

        let import_mcp = params
            .get("mcp_servers")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let import_skills = params
            .get("skills")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let import_memory = params
            .get("memory")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let mut categories = Vec::new();

        if import_mcp {
            categories.push(chelix_claude_import::mcp_servers::import_mcp_servers(
                &detection, &mcp_path,
            ));
        }
        if import_skills {
            categories.push(chelix_claude_import::skills::import_skills(
                &detection,
                &skills_dir,
            ));
        }
        if import_memory {
            categories.push(chelix_claude_import::memory::import_memory(
                &detection, &data_dir,
            ));
        }

        let total: usize = categories.iter().map(|c| c.items_imported).sum();

        Ok(serde_json::json!({
            "categories": categories,
            "total_imported": total,
        }))
    }

    #[cfg(not(feature = "claude-import"))]
    async fn claude_import(&self, _params: Value) -> ServiceResult {
        Err("claude import feature not enabled".into())
    }

    // ── Codex import ────────────────────────────────────────────────────────

    #[cfg(feature = "codex-import")]
    async fn codex_detect(&self) -> ServiceResult {
        let detection = chelix_codex_import::detect::detect();
        match detection {
            Some(d) => {
                let mcp_count = chelix_codex_import::mcp_servers::count_mcp_servers(&d);

                tracing::info!(
                    has_config = d.config_path.is_some(),
                    mcp_servers = mcp_count,
                    has_instructions = d.instructions_path.is_some(),
                    "codex.detect: installation detected"
                );

                Ok(serde_json::json!({
                    "detected": true,
                    "home_dir": d.home_dir.display().to_string(),
                    "has_mcp_servers": mcp_count > 0,
                    "mcp_servers_count": mcp_count,
                    "has_memory": d.instructions_path.is_some(),
                }))
            },
            None => {
                tracing::info!("codex.detect: no installation detected");
                Ok(serde_json::json!({ "detected": false }))
            },
        }
    }

    #[cfg(not(feature = "codex-import"))]
    async fn codex_detect(&self) -> ServiceResult {
        Ok(serde_json::json!({ "detected": false }))
    }

    #[cfg(feature = "codex-import")]
    async fn codex_import(&self, params: Value) -> ServiceResult {
        let detection = chelix_codex_import::detect::detect()
            .ok_or_else(|| "no Codex CLI installation found".to_string())?;

        let data_dir = chelix_config::data_dir();
        let mcp_path = data_dir.join("mcp-servers.json");

        let import_mcp = params
            .get("mcp_servers")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let import_memory = params
            .get("memory")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let mut categories = Vec::new();

        if import_mcp {
            categories.push(chelix_codex_import::mcp_servers::import_mcp_servers(
                &detection, &mcp_path,
            ));
        }
        if import_memory {
            categories.push(chelix_codex_import::memory::import_memory(
                &detection, &data_dir,
            ));
        }

        let total: usize = categories.iter().map(|c| c.items_imported).sum();

        Ok(serde_json::json!({
            "categories": categories,
            "total_imported": total,
        }))
    }

    #[cfg(not(feature = "codex-import"))]
    async fn codex_import(&self, _params: Value) -> ServiceResult {
        Err("codex import feature not enabled".into())
    }
}

fn parse_geo_location(value: &Value) -> Option<chelix_config::GeoLocation> {
    let latitude = value.get("latitude").and_then(|v| v.as_f64())?;
    let longitude = value.get("longitude").and_then(|v| v.as_f64())?;
    let place = value
        .get("place")
        .and_then(|v| v.as_str())
        .map(|v| v.to_string());
    let updated_at = value.get("updated_at").and_then(|v| v.as_i64());

    Some(chelix_config::GeoLocation {
        latitude,
        longitude,
        place,
        updated_at,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_geo_location_parses_valid_payload() {
        let parsed = parse_geo_location(&serde_json::json!({
            "latitude": 40.7128,
            "longitude": -74.0060,
            "place": "New York",
            "updated_at": 123,
        }))
        .expect("location should parse");

        assert_eq!(parsed.latitude, 40.7128);
        assert_eq!(parsed.longitude, -74.0060);
        assert_eq!(parsed.place.as_deref(), Some("New York"));
        assert_eq!(parsed.updated_at, Some(123));
    }

    #[test]
    fn parse_geo_location_rejects_invalid_payload() {
        assert!(parse_geo_location(&serde_json::json!({ "latitude": 40.7 })).is_none());
    }
}
