use {
    chelix_config::schema::ReasoningEffort,
    chelix_service_traits::{ServiceError, ServiceResult},
};

use crate::state::GatewayState;

pub async fn agent_defaults_for_agent(
    state: &GatewayState,
    agent_id: Option<&str>,
) -> ServiceResult<(String, ReasoningEffort)> {
    let agent_id = agent_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ServiceError::message("agent_id is required"))?;
    let agents_config = state
        .services
        .agents_config
        .as_ref()
        .ok_or_else(|| ServiceError::message("agent configuration is not available"))?;
    let guard = agents_config.read().await;
    let agent = guard
        .get(agent_id)
        .ok_or_else(|| ServiceError::message(format!("agent '{agent_id}' is not configured")))?;
    Ok((agent.model.clone(), agent.reasoning_effort.clone()))
}
