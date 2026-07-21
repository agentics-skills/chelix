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
        "Manage interactive terminal processes through tmux sessions owned by chelix-tools-service. Actions: start, poll, send_keys, paste, kill, list."
    }

    fn agent_result(
        &self,
        _params: &serde_json::Value,
        raw_result: &serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let response: ProcessResponse = serde_json::from_value(raw_result.clone())?;
        if !response.success {
            let error = response
                .error
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("failed process response has no error"))?;
            return Ok(serde_json::Value::String(format!(
                "Process request failed: {error}"
            )));
        }
        let status = match response.session_name.as_deref() {
            Some(session_name) => format!("Process session {session_name} updated."),
            None => "Process request completed.".to_string(),
        };
        let result = match response.output.as_deref() {
            Some(output) if !output.is_empty() => format!("{status}\nOutput:\n{output}"),
            _ => status,
        };
        Ok(serde_json::Value::String(result))
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["start", "poll", "send_keys", "paste", "kill", "list"],
                    "description": "The action to perform"
                },
                "command": {
                    "type": "string",
                    "description": "The command to run for start"
                },
                "session_name": {
                    "type": "string",
                    "description": "Tmux session name. Auto-generated for start; required for poll/send_keys/paste/kill."
                },
                "keys": {
                    "type": "string",
                    "description": "Keystrokes to send for send_keys"
                },
                "text": {
                    "type": "string",
                    "description": "Text to paste for paste"
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
            ProcessAction::Start { .. } => "start",
            ProcessAction::Poll { .. } => "poll",
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
            .await?;
        info!(
            session = session_key,
            action = action_label,
            success = response.success,
            "process tool completed"
        );

        #[cfg(feature = "metrics")]
        {
            counter!(
                tools_metrics::EXECUTIONS_TOTAL,
                labels::TOOL => "process".to_string(),
                labels::SUCCESS => response.success.to_string()
            )
            .increment(1);
            histogram!(
                tools_metrics::EXECUTION_DURATION_SECONDS,
                labels::TOOL => "process".to_string()
            )
            .record(start.elapsed().as_secs_f64());
            if !response.success {
                counter!(
                    tools_metrics::EXECUTION_ERRORS_TOTAL,
                    labels::TOOL => "process".to_string()
                )
                .increment(1);
            }
        }

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
            .with_body("{\"success\":true,\"output\":\"no active sessions\"}")
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

        assert_eq!(result["success"], true);
        assert_eq!(result["output"], "no active sessions");
        call.assert_async().await;
    }
}
