use std::sync::Arc;

#[cfg(feature = "metrics")]
use std::time::Instant;

use {
    async_trait::async_trait,
    chelix_agents::tool_registry::AgentTool,
    chelix_protocol::{ProcessAction, ProcessRequest, ProcessResponse},
    tracing::info,
};

#[cfg(feature = "metrics")]
use chelix_metrics::{counter, histogram, labels, tools as tools_metrics};

use crate::{params::without_null_params, tools_service::ManagedToolsService};

pub struct ProcessTool {
    service: Arc<ManagedToolsService>,
}

impl ProcessTool {
    #[must_use]
    pub fn new(service: Arc<ManagedToolsService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl AgentTool for ProcessTool {
    fn name(&self) -> &str {
        "process"
    }

    fn description(&self) -> &str {
        "Control terminals created by execute_command in the current session. Use execute_command to start work and read_terminal_output to read output. Actions: send_keys, paste, kill, list."
    }

    async fn agent_result(
        &self,
        _params: &serde_json::Value,
        raw_result: &serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let response: ProcessResponse = serde_json::from_value(raw_result.clone())?;
        let result = match response {
            ProcessResponse::SendKeys { terminal_id } => {
                format!("Keys sent to terminal {terminal_id}.")
            },
            ProcessResponse::Paste { terminal_id } => {
                format!("Text pasted into terminal {terminal_id}.")
            },
            ProcessResponse::Kill { terminal_id } => format!("Terminal {terminal_id} killed."),
            ProcessResponse::List { terminal_ids } if terminal_ids.is_empty() => {
                "No terminals in the current session.".into()
            },
            ProcessResponse::List { terminal_ids } => {
                format!(
                    "Terminals in the current session: {}",
                    terminal_ids.join(", ")
                )
            },
        };
        Ok(serde_json::Value::String(result))
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["send_keys", "paste", "kill", "list"],
                    "description": "Terminal control action to perform"
                },
                "terminalId": {
                    "type": "string",
                    "description": "Managed terminal id returned by execute_command. Required for send_keys, paste, and kill."
                },
                "keys": {
                    "type": "string",
                    "description": "RMUX key name or literal keystrokes. Required for send_keys."
                },
                "text": {
                    "type": "string",
                    "description": "Text to paste without interpreting it as key names. Required for paste."
                }
            },
            "required": ["action"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        #[cfg(feature = "metrics")]
        let start = Instant::now();

        let session_key = params
            .get("_session_key")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("main")
            .to_string();
        let action: ProcessAction = serde_json::from_value(without_null_params(params))?;
        let action_label = match &action {
            ProcessAction::SendKeys { .. } => "send_keys",
            ProcessAction::Paste { .. } => "paste",
            ProcessAction::Kill { .. } => "kill",
            ProcessAction::List => "list",
        };
        let response = self
            .service
            .process(&session_key, ProcessRequest {
                session_key: session_key.clone(),
                action,
            })
            .await;
        let success = response.is_ok();
        info!(
            session = session_key,
            action = action_label,
            success,
            "process tool completed"
        );

        #[cfg(feature = "metrics")]
        {
            counter!(
                tools_metrics::EXECUTIONS_TOTAL,
                labels::TOOL => "process".to_string(),
                labels::SUCCESS => success.to_string()
            )
            .increment(1);
            histogram!(
                tools_metrics::EXECUTION_DURATION_SECONDS,
                labels::TOOL => "process".to_string()
            )
            .record(start.elapsed().as_secs_f64());
            if !success {
                counter!(
                    tools_metrics::EXECUTION_ERRORS_TOTAL,
                    labels::TOOL => "process".to_string()
                )
                .increment(1);
            }
        }

        let response = response?;
        Ok(serde_json::to_value(response)?)
    }
}

#[cfg(test)]
mod tests {
    use crate::sandbox::ToolsServiceEndpoint;

    use super::*;

    #[tokio::test]
    async fn process_routes_to_service_with_session_scope() {
        let mut server = mockito::Server::new_async().await;
        let call = server
            .mock("POST", chelix_protocol::TOOLS_SERVICE_PROCESS_PATH)
            .match_header("authorization", "Bearer process-token")
            .match_body(mockito::Matcher::Json(serde_json::json!({
                "sessionKey": "session:test",
                "action": { "action": "list" }
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("{\"action\":\"list\",\"terminalIds\":[\"2\",\"4\"]}")
            .expect(1)
            .create_async()
            .await;
        let service = ManagedToolsService::for_test(ToolsServiceEndpoint {
            base_url: server.url(),
            token: "process-token".into(),
        })
        .unwrap_or_else(|error| panic!("test client failed: {error}"));
        let result = ProcessTool::new(service)
            .execute(serde_json::json!({
                "action": "list",
                "_session_key": "session:test"
            }))
            .await
            .unwrap_or_else(|error| panic!("process failed: {error}"));

        assert_eq!(result["action"], "list");
        assert_eq!(result["terminalIds"], serde_json::json!(["2", "4"]));
        assert_eq!(
            ProcessTool::new(client("http://127.0.0.1:1".into(), "unused"))
                .agent_result(&serde_json::json!({}), &result)
                .await
                .unwrap_or_else(|error| panic!("agent result failed: {error}")),
            "Terminals in the current session: 2, 4"
        );
        call.assert_async().await;
    }

    fn client(base_url: String, token: &str) -> Arc<ManagedToolsService> {
        ManagedToolsService::for_test(ToolsServiceEndpoint {
            base_url,
            token: token.into(),
        })
        .unwrap_or_else(|error| panic!("test client failed: {error}"))
    }

    #[test]
    fn schema_matches_project_multi_action_contract() {
        let schema =
            ProcessTool::new(client("http://127.0.0.1:1".into(), "unused")).parameters_schema();

        assert_eq!(
            schema,
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["send_keys", "paste", "kill", "list"],
                        "description": "Terminal control action to perform"
                    },
                    "terminalId": {
                        "type": "string",
                        "description": "Managed terminal id returned by execute_command. Required for send_keys, paste, and kill."
                    },
                    "keys": {
                        "type": "string",
                        "description": "RMUX key name or literal keystrokes. Required for send_keys."
                    },
                    "text": {
                        "type": "string",
                        "description": "Text to paste without interpreting it as key names. Required for paste."
                    }
                },
                "required": ["action"],
                "additionalProperties": false
            })
        );
    }
}
