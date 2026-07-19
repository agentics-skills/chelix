//! Wire types shared by the managed tools service and its client.

use serde::{Deserialize, Serialize};

pub const TOOLS_SERVICE_PROTOCOL_VERSION: u32 = 2;
pub const TOOLS_SERVICE_CONTAINER_PORT: u16 = 43_271;
pub const TOOLS_SERVICE_HEALTH_PATH: &str = "/v1/health";
pub const TOOLS_SERVICE_LIST_DIRECTORY_PATH: &str = "/v1/list-directory";
pub const TOOLS_SERVICE_RIPGREP_PATH: &str = "/v1/ripgrep";
pub const TOOLS_SERVICE_AUTH_HEADER: &str = "authorization";
pub const TOOLS_SERVICE_TOKEN_ENV: &str = "CHELIX_TOOLS_SERVICE_TOKEN";
pub const TOOLS_SERVICE_BINARY_ENV: &str = "CHELIX_TOOLS_SERVICE_BINARY";
pub const TOOLS_SERVICE_LINUX_BINARY_ENV: &str = "CHELIX_TOOLS_SERVICE_LINUX_BINARY";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsServiceReady {
    pub protocol_version: u32,
    pub port: u16,
    pub token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsServiceHealth {
    pub protocol_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListDirectoryRequest {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListDirectoryResponse {
    pub result: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RipgrepRequest {
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RipgrepResponse {
    pub result: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolsServiceError {
    pub error: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_message_round_trips() {
        let ready = ToolsServiceReady {
            protocol_version: TOOLS_SERVICE_PROTOCOL_VERSION,
            port: 31_337,
            token: "secret".into(),
        };
        let json = serde_json::to_string(&ready).unwrap_or_default();
        let decoded: ToolsServiceReady =
            serde_json::from_str(&json).unwrap_or_else(|error| panic!("decode failed: {error}"));

        assert_eq!(decoded, ready);
    }

    #[test]
    fn list_directory_messages_round_trip() {
        let request = ListDirectoryRequest {
            path: "/workspace".into(),
        };
        let request_json = serde_json::to_string(&request).unwrap_or_default();
        let decoded_request: ListDirectoryRequest = serde_json::from_str(&request_json)
            .unwrap_or_else(|error| panic!("request decode failed: {error}"));
        assert_eq!(decoded_request, request);

        let response = ListDirectoryResponse {
            result: "src/\nCargo.toml (1 line)".into(),
        };
        let response_json = serde_json::to_string(&response).unwrap_or_default();
        let decoded_response: ListDirectoryResponse = serde_json::from_str(&response_json)
            .unwrap_or_else(|error| panic!("response decode failed: {error}"));
        assert_eq!(decoded_response, response);
    }
}
