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
        self.inner
            .wizard_start(force)
            .map_err(ServiceError::message)
    }

    async fn wizard_next(&self, params: Value) -> ServiceResult {
        let input = params.get("input").and_then(|v| v.as_str()).unwrap_or("");
        let response = self
            .inner
            .wizard_next(input)
            .map_err(ServiceError::message)?;

        if response.get("done").and_then(Value::as_bool) == Some(true)
            && let (Some(agent_id), Some(agent_value)) = (
                response.get("agent_id").and_then(Value::as_str),
                response.get("agent"),
            )
        {
            let agent: chelix_config::AgentConfig =
                serde_json::from_value(agent_value.clone()).map_err(ServiceError::message)?;
            if let Some(state) = self.gateway_state.get()
                && let Some(agents) = state.services.agents_config.as_ref()
            {
                agents
                    .write()
                    .await
                    .entries
                    .insert(agent_id.to_string(), agent);
            }
        }

        Ok(response)
    }

    async fn wizard_cancel(&self) -> ServiceResult {
        self.inner.wizard_cancel();
        Ok(serde_json::json!({}))
    }

    async fn wizard_status(&self) -> ServiceResult {
        Ok(self.inner.wizard_status())
    }

    async fn user_get(&self) -> ServiceResult {
        self.inner.user_get().map_err(ServiceError::message)
    }

    async fn user_update(&self, params: Value) -> ServiceResult {
        let response = self
            .inner
            .user_update(params)
            .map_err(ServiceError::message)?;

        if let Some(state) = self.gateway_state.get() {
            let mut inner = state.inner.write().await;
            inner.cached_location = response.get("location").and_then(parse_geo_location);
        }

        Ok(response)
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
