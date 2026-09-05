//! Unified sub-agent coordination tool.

use std::sync::Arc;

use {
    async_trait::async_trait,
    chelix_agents::{tool_context::ToolExecutionContext, tool_registry::AgentTool},
    futures::future::BoxFuture,
    serde_json::Value,
};

use crate::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubAgentMode {
    Blocking,
    Background,
}

impl SubAgentMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Blocking => "blocking",
            Self::Background => "background",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubAgentRequest {
    Explore,
    Run {
        parent_session_key: String,
        agent_id: String,
        task: String,
        mode: SubAgentMode,
    },
    Status {
        parent_session_key: String,
        session_key: String,
    },
    List {
        parent_session_key: String,
    },
    Result {
        parent_session_key: String,
        session_key: String,
    },
    Cancel {
        parent_session_key: String,
        session_key: String,
    },
}

impl SubAgentRequest {
    #[must_use]
    pub fn action(&self) -> &'static str {
        match self {
            Self::Explore => "explore",
            Self::Run { .. } => "run",
            Self::Status { .. } => "status",
            Self::List { .. } => "list",
            Self::Result { .. } => "result",
            Self::Cancel { .. } => "cancel",
        }
    }
}

pub type SubAgentFn =
    Arc<dyn Fn(SubAgentRequest) -> BoxFuture<'static, crate::Result<Value>> + Send + Sync>;

pub struct SubAgentTool {
    execute_fn: SubAgentFn,
}

impl SubAgentTool {
    #[must_use]
    pub fn new(execute_fn: SubAgentFn) -> Self {
        Self { execute_fn }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubAgentAction {
    Explore,
    Run,
    Status,
    List,
    Result,
    Cancel,
}

impl SubAgentAction {
    fn parse(value: &str) -> crate::Result<Self> {
        match value {
            "explore" => Ok(Self::Explore),
            "run" => Ok(Self::Run),
            "status" => Ok(Self::Status),
            "list" => Ok(Self::List),
            "result" => Ok(Self::Result),
            "cancel" => Ok(Self::Cancel),
            _ => Err(Error::message(format!(
                "unsupported sub_agent action: {value}"
            ))),
        }
    }

    fn public_parameters(self) -> &'static [&'static str] {
        match self {
            Self::Explore | Self::List => &[],
            Self::Run => &["agent_id", "task", "mode"],
            Self::Status | Self::Result | Self::Cancel => &["session_key"],
        }
    }
}

fn object(params: &Value) -> crate::Result<&serde_json::Map<String, Value>> {
    params
        .as_object()
        .ok_or_else(|| Error::message("sub_agent parameters must be an object"))
}

fn action_payload(
    params: &serde_json::Map<String, Value>,
) -> crate::Result<(SubAgentAction, &serde_json::Map<String, Value>)> {
    let actions = params
        .get("action")
        .ok_or_else(|| Error::message("missing required parameter: action"))?
        .as_object()
        .ok_or_else(|| Error::message("parameter action must be an object"))?;
    if actions.len() != 1 {
        return Err(Error::message(format!(
            "parameter action must contain exactly one action, got: {}",
            actions.keys().cloned().collect::<Vec<_>>().join(", ")
        )));
    }
    let Some((name, payload)) = actions.iter().next() else {
        return Err(Error::message(
            "parameter action must contain exactly one action",
        ));
    };
    let action = SubAgentAction::parse(name)?;
    let payload = payload
        .as_object()
        .ok_or_else(|| Error::message(format!("parameter action.{name} must be an object")))?;
    Ok((action, payload))
}

fn required_string<'a>(
    params: &'a serde_json::Map<String, Value>,
    name: &str,
) -> crate::Result<&'a str> {
    let value = params
        .get(name)
        .ok_or_else(|| Error::message(format!("missing required parameter: {name}")))?
        .as_str()
        .ok_or_else(|| Error::message(format!("parameter {name} must be a string")))?;
    if value.trim().is_empty() {
        return Err(Error::message(format!(
            "parameter {name} must not be empty"
        )));
    }
    Ok(value)
}

fn validate_parameters(params: &Value) -> crate::Result<SubAgentAction> {
    let params = object(params)?;
    for name in params.keys() {
        if name != "action" {
            return Err(Error::message(format!("unknown parameter: {name}")));
        }
    }

    let (action, payload) = action_payload(params)?;
    let allowed = action.public_parameters();
    for name in payload.keys() {
        if !allowed.contains(&name.as_str()) {
            return Err(Error::message(format!("unknown parameter: {name}")));
        }
    }

    match action {
        SubAgentAction::Explore | SubAgentAction::List => {},
        SubAgentAction::Run => {
            required_string(payload, "agent_id")?;
            required_string(payload, "task")?;
            let mode = required_string(payload, "mode")?;
            if !matches!(mode, "blocking" | "background") {
                return Err(Error::message(format!(
                    "parameter mode must be either 'blocking' or 'background', got {mode:?}"
                )));
            }
        },
        SubAgentAction::Status | SubAgentAction::Result | SubAgentAction::Cancel => {
            required_string(payload, "session_key")?;
        },
    }
    Ok(action)
}

fn parent_session_key(context: Option<&ToolExecutionContext>) -> crate::Result<String> {
    context
        .ok_or_else(|| Error::message("session execution context is required"))?
        .require_session_key()
        .map(|key| key.as_str().to_owned())
        .map_err(|error| Error::message(error.to_string()))
}

fn parse_request(
    params: &Value,
    context: Option<&ToolExecutionContext>,
) -> crate::Result<SubAgentRequest> {
    let action = validate_parameters(params)?;
    let params = object(params)?;
    let (_, payload) = action_payload(params)?;
    match action {
        SubAgentAction::Explore => Ok(SubAgentRequest::Explore),
        SubAgentAction::Run => {
            let mode = match required_string(payload, "mode")? {
                "blocking" => SubAgentMode::Blocking,
                "background" => SubAgentMode::Background,
                value => {
                    return Err(Error::message(format!(
                        "parameter mode must be either 'blocking' or 'background', got {value:?}"
                    )));
                },
            };
            Ok(SubAgentRequest::Run {
                parent_session_key: parent_session_key(context)?,
                agent_id: required_string(payload, "agent_id")?.to_string(),
                task: required_string(payload, "task")?.to_string(),
                mode,
            })
        },
        SubAgentAction::Status => Ok(SubAgentRequest::Status {
            parent_session_key: parent_session_key(context)?,
            session_key: required_string(payload, "session_key")?.to_string(),
        }),
        SubAgentAction::List => Ok(SubAgentRequest::List {
            parent_session_key: parent_session_key(context)?,
        }),
        SubAgentAction::Result => Ok(SubAgentRequest::Result {
            parent_session_key: parent_session_key(context)?,
            session_key: required_string(payload, "session_key")?.to_string(),
        }),
        SubAgentAction::Cancel => Ok(SubAgentRequest::Cancel {
            parent_session_key: parent_session_key(context)?,
            session_key: required_string(payload, "session_key")?.to_string(),
        }),
    }
}

#[async_trait]
impl AgentTool for SubAgentTool {
    fn name(&self) -> &str {
        "sub_agent"
    }

    fn description(&self) -> &str {
        "Explore configured sub-agents, run one in a child session, and inspect or control direct child runs."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "action": {
                    "type": "object",
                    "description": "Exactly one action name and its strict parameter object.",
                    "oneOf": [
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "explore": {
                                    "type": "object",
                                    "description": "List configured agents available for delegated runs.",
                                    "additionalProperties": false,
                                    "properties": {},
                                    "required": []
                                }
                            },
                            "required": ["explore"]
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "run": {
                                    "type": "object",
                                    "description": "Start a delegated run.",
                                    "additionalProperties": false,
                                    "properties": {
                                        "agent_id": { "type": "string", "minLength": 1 },
                                        "task": { "type": "string", "minLength": 1 },
                                        "mode": {
                                            "type": "string",
                                            "pattern": "^(blocking|background)$"
                                        }
                                    },
                                    "required": ["agent_id", "task", "mode"]
                                }
                            },
                            "required": ["run"]
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "status": {
                                    "type": "object",
                                    "description": "Read direct-child status.",
                                    "additionalProperties": false,
                                    "properties": {
                                        "session_key": { "type": "string", "minLength": 1 }
                                    },
                                    "required": ["session_key"]
                                }
                            },
                            "required": ["status"]
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "list": {
                                    "type": "object",
                                    "description": "List direct child sessions.",
                                    "additionalProperties": false,
                                    "properties": {},
                                    "required": []
                                }
                            },
                            "required": ["list"]
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "result": {
                                    "type": "object",
                                    "description": "Read a completed background result.",
                                    "additionalProperties": false,
                                    "properties": {
                                        "session_key": { "type": "string", "minLength": 1 }
                                    },
                                    "required": ["session_key"]
                                }
                            },
                            "required": ["result"]
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "cancel": {
                                    "type": "object",
                                    "description": "Cancel a direct-child run.",
                                    "additionalProperties": false,
                                    "properties": {
                                        "session_key": { "type": "string", "minLength": 1 }
                                    },
                                    "required": ["session_key"]
                                }
                            },
                            "required": ["cancel"]
                        }
                    ]
                }
            },
            "required": ["action"]
        })
    }

    fn validate(&self, params: &Value) -> anyhow::Result<()> {
        validate_parameters(params).map(|_| ()).map_err(Into::into)
    }

    #[tracing::instrument(
        name = "sub_agent.execute",
        skip_all,
        fields(
            action = params
                .get("action")
                .and_then(|value| value.as_object())
                .and_then(|actions| actions.keys().next())
                .map(String::as_str)
                .unwrap_or("<invalid>")
        )
    )]
    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        let request = parse_request(&params, None)?;
        (self.execute_fn)(request).await.map_err(Into::into)
    }

    async fn execute_with_context(
        &self,
        params: Value,
        context: &ToolExecutionContext,
    ) -> anyhow::Result<Value> {
        let request = parse_request(&params, Some(context))?;
        (self.execute_fn)(request).await.map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use futures::FutureExt;

    use super::*;

    fn tool() -> SubAgentTool {
        SubAgentTool::new(Arc::new(|_| async { Ok(serde_json::json!({})) }.boxed()))
    }

    fn validation_error(params: Value) -> String {
        tool()
            .validate(&params)
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn actions_use_typed_session_context() -> anyhow::Result<()> {
        use chelix_sessions::SessionKey;

        let context = ToolExecutionContext::for_session(SessionKey::new("session:parent"));
        let cases = [
            (
                serde_json::json!({"action": {"explore": {}}}),
                SubAgentRequest::Explore,
            ),
            (
                serde_json::json!({"action": {"run": {
                    "agent_id": "reviewer", "task": "Do work", "mode": "blocking"
                }}}),
                SubAgentRequest::Run {
                    parent_session_key: "session:parent".into(),
                    agent_id: "reviewer".into(),
                    task: "Do work".into(),
                    mode: SubAgentMode::Blocking,
                },
            ),
            (
                serde_json::json!({"action": {"list": {}}}),
                SubAgentRequest::List {
                    parent_session_key: "session:parent".into(),
                },
            ),
            (
                serde_json::json!({"action": {"status": {"session_key": "session:child"}}}),
                SubAgentRequest::Status {
                    parent_session_key: "session:parent".into(),
                    session_key: "session:child".into(),
                },
            ),
            (
                serde_json::json!({"action": {"result": {"session_key": "session:child"}}}),
                SubAgentRequest::Result {
                    parent_session_key: "session:parent".into(),
                    session_key: "session:child".into(),
                },
            ),
            (
                serde_json::json!({"action": {"cancel": {"session_key": "session:child"}}}),
                SubAgentRequest::Cancel {
                    parent_session_key: "session:parent".into(),
                    session_key: "session:child".into(),
                },
            ),
        ];
        for (params, expected) in cases {
            let requires_context = expected != SubAgentRequest::Explore;
            let tool = SubAgentTool::new(Arc::new(move |request| {
                assert_eq!(request, expected);
                async { Ok(serde_json::json!({"accepted": true})) }.boxed()
            }));
            tool.validate(&params)?;
            assert_eq!(
                tool.execute_with_context(params.clone(), &context).await?,
                serde_json::json!({"accepted": true}),
            );
            if requires_context {
                assert!(tool.execute(params).await.is_err());
            } else {
                assert_eq!(
                    tool.execute(params).await?,
                    serde_json::json!({"accepted": true})
                );
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn rejects_additional_field_before_callback() {
        let tool = SubAgentTool::new(Arc::new(|_| panic!("invalid input reached callback")));
        let context =
            ToolExecutionContext::for_session(chelix_sessions::SessionKey::new("session:parent"));
        assert_eq!(tool.parameters_schema()["additionalProperties"], false);
        for params in [
            serde_json::json!({"action": {"explore": {}}, "extra_field": true}),
            serde_json::json!({"action": {"explore": {"extra_field": true}}}),
        ] {
            assert!(tool.validate(&params).is_err());
            assert!(tool.execute(params.clone()).await.is_err());
            assert!(tool.execute_with_context(params, &context).await.is_err());
        }
    }

    #[test]
    fn run_validation_rejects_missing_agent_id() {
        let error = validation_error(serde_json::json!({
            "action": {
                "run": {
                    "task": "Do work",
                    "mode": "blocking"
                }
            }
        }));
        assert!(error.to_string().contains("agent_id"));
    }

    #[test]
    fn run_validation_rejects_empty_task() {
        let error = validation_error(serde_json::json!({
            "action": {
                "run": {
                    "agent_id": "reviewer",
                    "task": "   ",
                    "mode": "blocking"
                }
            }
        }));
        assert!(error.to_string().contains("task"));
    }

    #[test]
    fn run_validation_rejects_missing_and_invalid_mode() {
        let missing = validation_error(serde_json::json!({
            "action": {
                "run": {
                    "agent_id": "reviewer",
                    "task": "Do work"
                }
            }
        }));
        assert!(missing.to_string().contains("mode"));

        let invalid = validation_error(serde_json::json!({
            "action": {
                "run": {
                    "agent_id": "reviewer",
                    "task": "Do work",
                    "mode": "async"
                }
            }
        }));
        assert!(invalid.to_string().contains("mode"));
    }
}
