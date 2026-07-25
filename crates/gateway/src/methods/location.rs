use chelix_protocol::{ErrorShape, error_codes};

use super::MethodRegistry;

pub(super) fn register(registry: &mut MethodRegistry) {
    registry.register(
        "location.result",
        Box::new(|context| {
            Box::pin(async move {
                let request_id = context
                    .params
                    .get("requestId")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        ErrorShape::new(error_codes::INVALID_REQUEST, "missing requestId")
                    })?;

                let result = if let Some(location) = context.params.get("location") {
                    if let (Some(latitude), Some(longitude)) = (
                        location.get("latitude").and_then(serde_json::Value::as_f64),
                        location
                            .get("longitude")
                            .and_then(serde_json::Value::as_f64),
                    ) {
                        let geolocation =
                            chelix_config::GeoLocation::now(latitude, longitude, None);
                        context.state.inner.write().await.cached_location =
                            Some(geolocation.clone());

                        let write_mode = chelix_config::discover_and_load()
                            .map_err(|error| {
                                ErrorShape::new(error_codes::INTERNAL, error.to_string())
                            })?
                            .memory
                            .user_profile_write_mode;
                        if write_mode.allows_auto_write() {
                            let mut user =
                                chelix_config::resolve_user_profile().map_err(|error| {
                                    ErrorShape::new(error_codes::INTERNAL, error.to_string())
                                })?;
                            user.location = Some(geolocation);
                            if let Err(error) =
                                chelix_config::save_user_with_mode(&user, write_mode)
                            {
                                tracing::warn!(%error, "failed to persist location to USER.md");
                            }
                        }
                    }
                    serde_json::json!({ "location": context.params.get("location") })
                } else {
                    serde_json::json!({ "error": context.params.get("error") })
                };

                let pending = context
                    .state
                    .inner
                    .write()
                    .await
                    .pending_location_requests
                    .remove(request_id);
                if let Some(request) = pending {
                    let _ = request.sender.send(result);
                    Ok(serde_json::json!({}))
                } else {
                    Err(ErrorShape::new(
                        error_codes::INVALID_REQUEST,
                        "no pending location request for this id",
                    ))
                }
            })
        }),
    );
}
