//! Wire types for the remote embedding provider.

use serde::{Deserialize, Serialize};

pub const EMBEDDING_SERVICE_EMBED_PATH: &str = "/v1/embed";

/// Background index jobs. Lower than [`EMBEDDING_PRIORITY_SEARCH`].
pub const EMBEDDING_PRIORITY_INDEX: u32 = 0;
/// Interactive search jobs. Served before waiting index jobs.
pub const EMBEDDING_PRIORITY_SEARCH: u32 = 1;
/// Maximum JSON body size accepted by `POST /v1/embed`.
pub const EMBEDDING_MAX_BODY_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingRequest {
    pub text: String,
    #[serde(default)]
    pub priority: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbeddingResponse {
    pub embedding: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingServiceError {
    pub error: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embed_request_round_trips_priority() {
        let request = EmbeddingRequest {
            text: "hello".into(),
            priority: EMBEDDING_PRIORITY_SEARCH,
        };

        let json = serde_json::to_string(&request).unwrap_or_else(|error| panic!("{error}"));
        let decoded: EmbeddingRequest =
            serde_json::from_str(&json).unwrap_or_else(|error| panic!("decode failed: {error}"));

        assert_eq!(decoded, request);
    }

    #[test]
    fn embed_request_defaults_missing_priority_to_index() {
        let decoded: EmbeddingRequest = serde_json::from_str(r#"{"text":"hello"}"#)
            .unwrap_or_else(|error| panic!("decode failed: {error}"));

        assert_eq!(decoded.text, "hello");
        assert_eq!(decoded.priority, EMBEDDING_PRIORITY_INDEX);
    }
}
