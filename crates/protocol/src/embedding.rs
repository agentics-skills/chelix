//! Wire types shared by the local embedding sidecar and its client.

use serde::{Deserialize, Serialize};

pub const EMBEDDING_SERVICE_PROTOCOL_VERSION: u32 = 1;
pub const EMBEDDING_SERVICE_EMBED_PATH: &str = "/v1/embed";
pub const EMBEDDING_SERVICE_HEALTH_PATH: &str = "/health";

/// Background index jobs. Lower than [`EMBEDDING_PRIORITY_SEARCH`].
pub const EMBEDDING_PRIORITY_INDEX: u32 = 0;
/// Interactive search jobs. Served before waiting index jobs.
pub const EMBEDDING_PRIORITY_SEARCH: u32 = 1;
/// Maximum JSON body size accepted by `POST /v1/embed`.
pub const EMBEDDING_MAX_BODY_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingModelMetadata {
    pub model_name: String,
    pub dimensions: usize,
    pub provider_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingServiceReady {
    pub protocol_version: u32,
    pub port: u16,
    pub model: EmbeddingModelMetadata,
}

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
    fn ready_message_round_trips() {
        let ready = EmbeddingServiceReady {
            protocol_version: EMBEDDING_SERVICE_PROTOCOL_VERSION,
            port: 31_337,
            model: EmbeddingModelMetadata {
                model_name: "embeddinggemma".into(),
                dimensions: 768,
                provider_key: "local-mistral:q8:test-model:0123456789abcdef".into(),
            },
        };

        let json = serde_json::to_string(&ready).unwrap_or_default();
        let decoded: EmbeddingServiceReady =
            serde_json::from_str(&json).unwrap_or_else(|error| panic!("decode failed: {error}"));

        assert_eq!(decoded, ready);
    }

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
