use {
    chelix_common::ActiveToolInvocation,
    chelix_protocol::{ErrorShape, error_codes},
    serde::Deserialize,
    serde_json::Value,
};

use crate::a2ui::{
    A2uiClientMessage, BrokerSubmitError, InteractionKey, TOOL_NAME, surface_id_from_tool_arguments,
};

use super::MethodRegistry;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SubmitActionParams {
    #[serde(rename = "runId")]
    run_id: String,
    #[serde(rename = "toolCallId")]
    tool_call_id: String,
    message: Value,
}

pub(super) fn register(registry: &mut MethodRegistry) {
    registry.register(
        "a2ui.action",
        Box::new(|context| {
            Box::pin(async move {
                let params: SubmitActionParams = serde_json::from_value(context.params.clone())
                    .map_err(|error| {
                        ErrorShape::new(
                            error_codes::INVALID_REQUEST,
                            format!("invalid a2ui.action parameters: {error}"),
                        )
                    })?;
                let message = A2uiClientMessage::parse(params.message).map_err(|error| {
                    ErrorShape::new(error_codes::INVALID_REQUEST, error.to_string())
                })?;
                let session_key = context
                    .state
                    .client_registry
                    .read()
                    .await
                    .active_sessions
                    .get(&context.client_conn_id)
                    .cloned()
                    .ok_or_else(|| {
                        ErrorShape::new(
                            error_codes::CONFLICT,
                            "the client has no active chat session",
                        )
                    })?;

                let active_calls = context
                    .state
                    .chat()
                    .active_tool_invocations(&session_key)
                    .await;
                validate_active_interaction(
                    &active_calls,
                    &params.run_id,
                    &params.tool_call_id,
                    &message.action.surface_id,
                )?;

                let key = InteractionKey {
                    session_key,
                    run_id: params.run_id,
                    tool_call_id: params.tool_call_id,
                };
                context
                    .state
                    .a2ui_broker
                    .submit(key, message)
                    .map_err(map_submit_error)?;
                Ok(serde_json::json!({ "accepted": true }))
            })
        }),
    );
}

fn validate_active_interaction(
    active_calls: &[ActiveToolInvocation],
    run_id: &str,
    tool_call_id: &str,
    surface_id: &str,
) -> Result<(), ErrorShape> {
    let Some(call) = active_calls.iter().find(|call| {
        call.lifecycle.run_id.as_deref() == Some(run_id)
            && call.lifecycle.tool_call_id == tool_call_id
    }) else {
        return Err(ErrorShape::new(
            error_codes::CONFLICT,
            "the referenced A2UI tool call is not active in this session",
        ));
    };
    if call.lifecycle.tool_name != TOOL_NAME {
        return Err(ErrorShape::new(
            error_codes::CONFLICT,
            "the referenced active tool call is not render_a2ui",
        ));
    }
    let announced_surface = call
        .arguments()
        .ok_or_else(|| {
            ErrorShape::new(
                error_codes::INTERNAL,
                "active render_a2ui call has no arguments",
            )
        })
        .and_then(|arguments| {
            surface_id_from_tool_arguments(arguments).map_err(|error| {
                ErrorShape::new(
                    error_codes::INTERNAL,
                    format!("active render_a2ui call is invalid: {error}"),
                )
            })
        })?;
    if announced_surface != surface_id {
        return Err(ErrorShape::new(
            error_codes::CONFLICT,
            "the A2UI action surface does not match the active interaction",
        ));
    }
    Ok(())
}

fn map_submit_error(error: BrokerSubmitError) -> ErrorShape {
    let code = match error {
        BrokerSubmitError::BufferFull => error_codes::RATE_LIMITED,
        BrokerSubmitError::Duplicate | BrokerSubmitError::Closed => error_codes::CONFLICT,
    };
    ErrorShape::new(code, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active_call() -> ActiveToolInvocation {
        ActiveToolInvocation {
            lifecycle: chelix_common::tool_lifecycle::ToolLifecycleEvent {
                tool_call_id: "call-1".to_owned(),
                tool_name: TOOL_NAME.to_owned(),
                sequence: 4,
                emitted_at_ms: 1,
                run_id: Some("run-1".to_owned()),
                context_budget: None,
                update: chelix_common::tool_lifecycle::ToolLifecycleUpdate::Executing {
                    arguments: serde_json::json!({
                        "messages": [
                            {
                                "version": "v0.9.1",
                                "createSurface": {
                                    "surfaceId": "surface-1",
                                    "catalogId": crate::a2ui::BASIC_CATALOG_ID
                                }
                            },
                            {
                                "version": "v0.9.1",
                                "updateComponents": {
                                    "surfaceId": "surface-1",
                                    "components": [
                                        {
                                            "id": "root",
                                            "component": "Button",
                                            "child": "submit-label",
                                            "action": { "event": { "name": "submit" } }
                                        },
                                        {
                                            "id": "submit-label",
                                            "component": "Text",
                                            "text": "Submit"
                                        }
                                    ]
                                }
                            }
                        ]
                    }),
                    started_at_ms: 1,
                },
            },
            execution_mode: None,
            accumulated_arguments: None,
            context_budget: None,
        }
    }

    #[test]
    fn validates_exact_active_surface() {
        assert!(
            validate_active_interaction(&[active_call()], "run-1", "call-1", "surface-1").is_ok()
        );
    }

    #[test]
    fn rejects_other_run_and_surface() {
        assert!(
            validate_active_interaction(&[active_call()], "run-2", "call-1", "surface-1").is_err()
        );
        assert!(
            validate_active_interaction(&[active_call()], "run-1", "call-1", "surface-2").is_err()
        );
    }
}
