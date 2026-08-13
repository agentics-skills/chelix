use chelix_protocol::{ToolsServiceTerminalAttachQuery, ToolsServiceToolCallTerminalAttachQuery};

pub(crate) const TERMINAL_DISABLED: &str = "TERMINAL_DISABLED";
pub(crate) const TERMINAL_SERVICE_UNAVAILABLE: &str = "TERMINAL_SERVICE_UNAVAILABLE";
pub(crate) const TERMINAL_REQUEST_FAILED: &str = "TERMINAL_REQUEST_FAILED";

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSessionQuery {
    pub(crate) session_key: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateTerminalRequest {
    pub(crate) session_key: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(untagged)]
pub enum TerminalWsQuery {
    Terminal(TerminalAttachWsQuery),
    ToolCall(ToolCallTerminalWsQuery),
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalAttachWsQuery {
    pub(crate) instance_id: String,
    pub(crate) id: String,
    pub(crate) session_key: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolCallTerminalWsQuery {
    pub(crate) tool_call_id: String,
    pub(crate) session_key: String,
}

impl From<TerminalAttachWsQuery> for ToolsServiceTerminalAttachQuery {
    fn from(query: TerminalAttachWsQuery) -> Self {
        Self {
            id: query.id,
            session_key: query.session_key,
        }
    }
}

impl From<ToolCallTerminalWsQuery> for ToolsServiceToolCallTerminalAttachQuery {
    fn from(query: ToolCallTerminalWsQuery) -> Self {
        Self {
            tool_call_id: query.tool_call_id,
            session_key: query.session_key,
        }
    }
}

pub(crate) fn terminal_error(code: &str, error: impl Into<String>) -> serde_json::Value {
    serde_json::json!({
        "code": code,
        "error": error.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_websocket_query_accepts_only_one_exact_attachment_mode() {
        let terminal = serde_json::from_value::<TerminalWsQuery>(serde_json::json!({
            "instanceId": "instance-1",
            "id": "7",
            "sessionKey": "session:one"
        }));
        assert!(matches!(terminal, Ok(TerminalWsQuery::Terminal(_))));

        let tool_call = serde_json::from_value::<TerminalWsQuery>(serde_json::json!({
            "toolCallId": "call-1",
            "sessionKey": "session:one"
        }));
        assert!(matches!(tool_call, Ok(TerminalWsQuery::ToolCall(_))));

        for invalid in [
            serde_json::json!({ "sessionKey": "session:one" }),
            serde_json::json!({
                "instanceId": "instance-1",
                "id": "7",
                "toolCallId": "call-1",
                "sessionKey": "session:one"
            }),
            serde_json::json!({
                "toolCallId": "call-1",
                "sessionKey": "session:one",
                "terminalId": "7"
            }),
        ] {
            assert!(serde_json::from_value::<TerminalWsQuery>(invalid).is_err());
        }
    }
}
