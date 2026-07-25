use std::sync::Arc;

use {
    axum::{
        Json, Router,
        extract::State,
        http::StatusCode,
        response::{IntoResponse, Response},
        routing::{get, post},
    },
    chelix_protocol::{
        EMBEDDING_SERVICE_EMBED_PATH, EMBEDDING_SERVICE_HEALTH_PATH, EmbeddingModelMetadata,
        EmbeddingRequest, EmbeddingResponse, EmbeddingServiceError,
    },
};

use crate::EmbeddingEngine;

#[derive(Clone)]
struct ApiState {
    engine: Arc<dyn EmbeddingEngine>,
}

pub fn router(engine: Arc<dyn EmbeddingEngine>) -> Router {
    Router::new()
        .route(EMBEDDING_SERVICE_HEALTH_PATH, get(health))
        .route(EMBEDDING_SERVICE_EMBED_PATH, post(embed))
        .with_state(ApiState { engine })
}

async fn health(State(state): State<ApiState>) -> Json<EmbeddingModelMetadata> {
    Json(state.engine.metadata().clone())
}

#[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
async fn embed(State(state): State<ApiState>, Json(request): Json<EmbeddingRequest>) -> Response {
    #[cfg(feature = "metrics")]
    metrics::counter!("chelix_embedding_service_requests_total").increment(1);

    match state.engine.embed(&request.text).await {
        Ok(embedding) => Json(EmbeddingResponse { embedding }).into_response(),
        Err(error) => {
            #[cfg(feature = "metrics")]
            metrics::counter!("chelix_embedding_service_errors_total").increment(1);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(EmbeddingServiceError {
                    error: error.to_string(),
                }),
            )
                .into_response()
        },
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use {
        anyhow::{Result, bail},
        async_trait::async_trait,
        chelix_protocol::{EmbeddingRequest, EmbeddingResponse},
    };

    use super::*;

    struct FakeEngine {
        metadata: EmbeddingModelMetadata,
        fail: bool,
    }

    #[async_trait]
    impl EmbeddingEngine for FakeEngine {
        async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            if self.fail {
                bail!("synthetic embedding failure");
            }
            Ok(vec![1.0, 2.0, 3.0])
        }

        fn metadata(&self) -> &EmbeddingModelMetadata {
            &self.metadata
        }
    }

    fn fake_engine(fail: bool) -> Arc<dyn EmbeddingEngine> {
        Arc::new(FakeEngine {
            metadata: EmbeddingModelMetadata {
                model_name: "test-model".into(),
                dimensions: 3,
                provider_key: "local-gguf:test-model.gguf".into(),
            },
            fail,
        })
    }

    async fn spawn_api(fail: bool) -> String {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap_or_else(|error| panic!("bind failed: {error}"));
        let address = listener
            .local_addr()
            .unwrap_or_else(|error| panic!("local address failed: {error}"));
        tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router(fake_engine(fail))).await {
                panic!("test server failed: {error}");
            }
        });
        format!("http://{address}")
    }

    #[tokio::test]
    async fn embed_endpoint_requires_no_authorization() {
        let base_url = spawn_api(false).await;
        let response = reqwest::Client::new()
            .post(format!("{base_url}{EMBEDDING_SERVICE_EMBED_PATH}"))
            .json(&EmbeddingRequest {
                text: "hello".into(),
            })
            .send()
            .await
            .unwrap_or_else(|error| panic!("request failed: {error}"));

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .json::<EmbeddingResponse>()
            .await
            .unwrap_or_else(|error| panic!("response decode failed: {error}"));
        assert_eq!(body.embedding, vec![1.0, 2.0, 3.0]);
    }

    #[tokio::test]
    async fn engine_errors_are_reported_as_json() {
        let base_url = spawn_api(true).await;
        let response = reqwest::Client::new()
            .post(format!("{base_url}{EMBEDDING_SERVICE_EMBED_PATH}"))
            .json(&EmbeddingRequest {
                text: "hello".into(),
            })
            .send()
            .await
            .unwrap_or_else(|error| panic!("request failed: {error}"));

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = response
            .json::<EmbeddingServiceError>()
            .await
            .unwrap_or_else(|error| panic!("response decode failed: {error}"));
        assert!(body.error.contains("synthetic embedding failure"));
    }
}
