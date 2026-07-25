use serde_json::{Map, Value};

use crate::state::GatewayState;

pub async fn preset_defaults_for_agent(
    state: &GatewayState,
    agent_id: Option<&str>,
) -> (Option<String>, Option<String>) {
    let Some(agent_id) = agent_id.map(str::trim).filter(|value| !value.is_empty()) else {
        return (None, None);
    };
    let Some(agents_config) = state.services.agents_config.as_ref() else {
        return (None, None);
    };
    let guard = agents_config.read().await;
    let Some(preset) = guard.presets.get(agent_id) else {
        return (None, None);
    };
    (
        preset.model.clone(),
        preset
            .reasoning_effort
            .as_ref()
            .map(|effort| effort.as_str().to_string()),
    )
}

pub(crate) async fn enrich_session_entry_for_ui(
    state: &GatewayState,
    entry: &mut Map<String, Value>,
) {
    let stored_model = entry
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let stored_reasoning = entry
        .get("reasoningEffort")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let agent_id = entry
        .get("agent_id")
        .or_else(|| entry.get("agentId"))
        .and_then(Value::as_str);
    let (preset_model, preset_reasoning) = preset_defaults_for_agent(state, agent_id).await;

    if stored_model.is_none()
        && let Some(model) = preset_model
    {
        entry.insert("model".to_string(), Value::String(model));
    }

    let resolved_reasoning = stored_reasoning.or(preset_reasoning);
    if let Some(reasoning) = resolved_reasoning {
        entry.insert("reasoningEffort".to_string(), Value::String(reasoning));
    }
}

pub(crate) async fn materialize_agent_preset_session_defaults(
    state: &GatewayState,
    session_key: &str,
    agent_id: &str,
) {
    let Some(metadata) = state.services.session_metadata.as_ref() else {
        return;
    };
    let (preset_model, preset_reasoning) = preset_defaults_for_agent(state, Some(agent_id)).await;
    if let Some(model) = preset_model {
        metadata.set_model(session_key, Some(model)).await;
    }
    if preset_reasoning.is_some() {
        metadata
            .set_reasoning_effort(session_key, preset_reasoning)
            .await;
    }
}
