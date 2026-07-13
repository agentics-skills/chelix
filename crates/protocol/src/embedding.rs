//! Wire types shared by the local embedding sidecar and its client.

use serde::{Deserialize, Serialize};

pub const EMBEDDING_SERVICE_PROTOCOL_VERSION: u32 = 1;
pub const EMBEDDING_SERVICE_EMBED_PATH: &str = "/v1/embed";
pub const EMBEDDING_SERVICE_HEALTH_PATH: &str = "/health";

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
                provider_key: "local-gguf:model.gguf".into(),
            },
        };

        let json = serde_json::to_string(&ready).unwrap_or_default();
        let decoded: EmbeddingServiceReady =
            serde_json::from_str(&json).unwrap_or_else(|error| panic!("decode failed: {error}"));

        assert_eq!(decoded, ready);
    }
}
