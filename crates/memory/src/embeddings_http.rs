//! HTTP client for a remote embedding provider.

use std::time::Duration;

use {
    async_trait::async_trait,
    chelix_common::http_client::apply_proxy,
    chelix_protocol::{
        EMBEDDING_MAX_BODY_BYTES, EMBEDDING_PRIORITY_INDEX, EMBEDDING_SERVICE_EMBED_PATH,
        EmbeddingRequest, EmbeddingResponse, EmbeddingServiceError,
    },
    secrecy::{ExposeSecret, Secret},
};

use crate::embeddings::EmbeddingProvider;

pub struct HttpEmbeddingProvider {
    client: reqwest::Client,
    embed_url: String,
    endpoint: String,
    api_key: Secret<String>,
    dimensions: usize,
    provider_key: String,
}

impl HttpEmbeddingProvider {
    pub fn new(
        endpoint: String,
        api_key: Secret<String>,
        dimensions: usize,
    ) -> crate::error::Result<Self> {
        let base = endpoint.trim_end_matches('/');
        if base.is_empty() {
            return Err(crate::error::Error::Embedding(
                "embedding url is empty".into(),
            ));
        }
        if dimensions == 0 {
            return Err(crate::error::Error::Embedding(
                "embedding dimensions must be >= 1".into(),
            ));
        }
        let client =
            apply_proxy(reqwest::Client::builder().connect_timeout(Duration::from_secs(5)))
                .build()?;
        Ok(Self {
            client,
            embed_url: format!("{base}{EMBEDDING_SERVICE_EMBED_PATH}"),
            provider_key: format!("{endpoint}\n{dimensions}"),
            endpoint,
            api_key,
            dimensions,
        })
    }
}

#[async_trait]
impl EmbeddingProvider for HttpEmbeddingProvider {
    async fn embed(&self, text: &str) -> crate::error::Result<Vec<f32>> {
        self.embed_with_priority(text, EMBEDDING_PRIORITY_INDEX)
            .await
    }

    async fn embed_with_priority(
        &self,
        text: &str,
        priority: u32,
    ) -> crate::error::Result<Vec<f32>> {
        let response = self
            .client
            .post(&self.embed_url)
            .bearer_auth(self.api_key.expose_secret())
            .json(&EmbeddingRequest {
                text: text.to_owned(),
                priority,
            })
            .send()
            .await?;
        if !response.status().is_success() {
            let status = response.status();
            let error = response
                .json::<EmbeddingServiceError>()
                .await
                .map(|body| body.error)
                .unwrap_or_else(|decode_error| decode_error.to_string());
            return Err(crate::error::Error::Embedding(format!(
                "embedding provider returned {status}: {error}"
            )));
        }
        let response = response.json::<EmbeddingResponse>().await?;
        if response.embedding.len() != self.dimensions {
            return Err(crate::error::Error::Embedding(format!(
                "embedding provider returned {} dimensions; expected {}",
                response.embedding.len(),
                self.dimensions
            )));
        }
        Ok(response.embedding)
    }

    fn model_name(&self) -> &str {
        &self.endpoint
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn provider_key(&self) -> &str {
        &self.provider_key
    }

    fn max_embed_payload_bytes(&self) -> Option<usize> {
        Some(EMBEDDING_MAX_BODY_BYTES)
    }
}

#[cfg(test)]
mod tests {
    use chelix_protocol::EMBEDDING_PRIORITY_SEARCH;

    use super::*;

    fn provider(base_url: String) -> HttpEmbeddingProvider {
        HttpEmbeddingProvider::new(base_url, Secret::new("secret".into()), 3)
            .unwrap_or_else(|error| panic!("provider creation failed: {error}"))
    }

    #[tokio::test]
    async fn embed_sends_bearer_and_index_priority() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", EMBEDDING_SERVICE_EMBED_PATH)
            .match_header("authorization", "Bearer secret")
            .match_body(mockito::Matcher::JsonString(
                serde_json::json!({
                    "text": "hello",
                    "priority": EMBEDDING_PRIORITY_INDEX,
                })
                .to_string(),
            ))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"embedding":[1.0,2.0,3.0]}"#)
            .create_async()
            .await;
        let endpoint = server.url();
        let embedder = provider(endpoint.clone());

        let embedding = embedder
            .embed("hello")
            .await
            .unwrap_or_else(|error| panic!("embedding failed: {error}"));

        assert_eq!(embedding, vec![1.0, 2.0, 3.0]);
        assert_eq!(embedder.model_name(), endpoint);
        assert_eq!(embedder.provider_key(), format!("{endpoint}\n3"));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn embed_rejects_wrong_dimensions() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", EMBEDDING_SERVICE_EMBED_PATH)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"embedding":[1.0]}"#)
            .create_async()
            .await;
        let embedder = provider(server.url());

        let result = embedder.embed("hello").await;

        assert!(result.is_err());
        assert!(
            result
                .err()
                .is_some_and(|error| error.to_string().contains("returned 1 dimensions"))
        );
    }

    #[tokio::test]
    async fn embed_with_priority_sends_search_priority() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", EMBEDDING_SERVICE_EMBED_PATH)
            .match_header("authorization", "Bearer secret")
            .match_body(mockito::Matcher::JsonString(
                serde_json::json!({
                    "text": "hello",
                    "priority": EMBEDDING_PRIORITY_SEARCH,
                })
                .to_string(),
            ))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"embedding":[1.0,2.0,3.0]}"#)
            .create_async()
            .await;
        let embedder = provider(server.url());

        let embedding = embedder
            .embed_with_priority("hello", EMBEDDING_PRIORITY_SEARCH)
            .await
            .unwrap_or_else(|error| panic!("embedding failed: {error}"));

        assert_eq!(embedding, vec![1.0, 2.0, 3.0]);
        mock.assert_async().await;
    }
}
