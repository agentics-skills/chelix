use chelix_protocol::ToolsServiceTerminalAttachQuery;

pub(crate) const TERMINAL_DISABLED: &str = "TERMINAL_DISABLED";
pub(crate) const TERMINAL_SERVICE_UNAVAILABLE: &str = "TERMINAL_SERVICE_UNAVAILABLE";
pub(crate) const TERMINAL_REQUEST_FAILED: &str = "TERMINAL_REQUEST_FAILED";

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSessionQuery {
    pub(crate) session_key: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalWsQuery {
    pub(crate) instance_id: String,
    pub(crate) id: String,
    pub(crate) session_key: String,
}

impl From<TerminalWsQuery> for ToolsServiceTerminalAttachQuery {
    fn from(query: TerminalWsQuery) -> Self {
        Self {
            id: query.id,
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
