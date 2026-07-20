use std::sync::Arc;

use tracing::{info, warn};

use chelix_channels::ChannelReplyTarget;

use crate::state::GatewayState;

use super::super::{default_channel_session_key, resolve_channel_session};

fn persist_location_from_config(
    config: &chelix_config::ChelixConfig,
    geo: chelix_config::GeoLocation,
) {
    let write_mode = config.memory.user_profile_write_mode;
    if !write_mode.allows_auto_write() {
        return;
    }
    let mut user = chelix_config::resolve_user_profile_from_config(config);
    user.location = Some(geo);
    if let Err(error) = chelix_config::save_user_with_mode(&user, write_mode) {
        warn!(%error, "failed to persist location to USER.md");
    }
}

pub(in crate::channel_events) async fn update_location(
    state: &Arc<tokio::sync::OnceCell<Arc<GatewayState>>>,
    reply_to: &ChannelReplyTarget,
    latitude: f64,
    longitude: f64,
) -> bool {
    let Some(state) = state.get() else {
        warn!("update_location: gateway not ready");
        return false;
    };

    let session_key = if let Some(ref sm) = state.services.session_metadata {
        resolve_channel_session(reply_to, sm).await
    } else {
        default_channel_session_key(reply_to)
    };

    let config = match chelix_config::discover_and_load() {
        Ok(config) => config,
        Err(error) => {
            warn!(%error, "failed to load config while updating location");
            return false;
        },
    };

    let geo = chelix_config::GeoLocation::now(latitude, longitude, None);
    persist_location_from_config(&config, geo.clone());
    state.inner.write().await.cached_location = Some(geo.clone());

    // Check for a pending tool-triggered location request.
    let pending_key = format!("channel_location:{session_key}");
    let pending = state
        .inner
        .write()
        .await
        .pending_invokes
        .remove(&pending_key);
    if let Some(invoke) = pending {
        let result = serde_json::json!({
            "location": {
                "latitude": latitude,
                "longitude": longitude,
                "accuracy": 0.0,
            }
        });
        let _ = invoke.sender.send(result);
        info!(session_key, "resolved pending channel location request");
        return true;
    }

    false
}

pub(in crate::channel_events) async fn resolve_pending_location(
    state: &Arc<tokio::sync::OnceCell<Arc<GatewayState>>>,
    reply_to: &ChannelReplyTarget,
    latitude: f64,
    longitude: f64,
) -> bool {
    let Some(state) = state.get() else {
        warn!("resolve_pending_location: gateway not ready");
        return false;
    };

    let session_key = if let Some(ref sm) = state.services.session_metadata {
        resolve_channel_session(reply_to, sm).await
    } else {
        default_channel_session_key(reply_to)
    };
    let config = match chelix_config::discover_and_load() {
        Ok(config) => config,
        Err(error) => {
            warn!(%error, "failed to load config while resolving location");
            return false;
        },
    };

    // Only resolve if a pending tool-triggered location request exists.
    let pending_key = format!("channel_location:{session_key}");
    let pending = state
        .inner
        .write()
        .await
        .pending_invokes
        .remove(&pending_key);
    if let Some(invoke) = pending {
        // Cache and persist only when we resolved an explicit request.
        let geo = chelix_config::GeoLocation::now(latitude, longitude, None);
        persist_location_from_config(&config, geo.clone());
        state.inner.write().await.cached_location = Some(geo.clone());

        let result = serde_json::json!({
            "location": {
                "latitude": latitude,
                "longitude": longitude,
                "accuracy": 0.0,
            }
        });
        let _ = invoke.sender.send(result);
        info!(
            session_key,
            "resolved pending channel location request from text input"
        );
        return true;
    }

    false
}
