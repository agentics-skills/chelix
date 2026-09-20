use std::sync::Arc;

use {
    anyhow::Error,
    axum::{
        BoxError, Json, Router,
        error_handling::HandleErrorLayer,
        extract::{DefaultBodyLimit, State},
        http::StatusCode,
        response::{IntoResponse, Response},
        routing::{get, post},
    },
    chelix_protocol::{
        EMBEDDING_SERVICE_EMBED_PATH, EMBEDDING_SERVICE_HEALTH_PATH, EmbeddingModelMetadata,
        EmbeddingRequest, EmbeddingResponse, EmbeddingServiceError,
    },
    tower::{ServiceBuilder, limit::ConcurrencyLimitLayer, load_shed::LoadShedLayer},
};

use crate::{
    EmbeddingEngine,
    queue::{MAX_EMBED_BODY_BYTES, MAX_HTTP_IN_FLIGHT, MAX_WAITING_JOBS, QueueFull, QueuedEngine},
};

#[derive(Clone)]
struct ApiState {
    engine: Arc<dyn EmbeddingEngine>,
}

pub fn router(engine: Arc<dyn EmbeddingEngine>) -> Router {
    router_with_limits(engine, MAX_WAITING_JOBS, MAX_HTTP_IN_FLIGHT)
}

fn router_with_limits(
    engine: Arc<dyn EmbeddingEngine>,
    max_waiting: usize,
    max_http_in_flight: usize,
) -> Router {
    let queued = QueuedEngine::start(engine, max_waiting);
    let embed_routes = Router::new()
        .route(EMBEDDING_SERVICE_EMBED_PATH, post(embed))
        .layer(
            ServiceBuilder::new()
                .layer(HandleErrorLayer::new(|error: BoxError| async move {
                    overload_response(error)
                }))
                .layer(LoadShedLayer::new())
                .layer(ConcurrencyLimitLayer::new(max_http_in_flight))
                .layer(DefaultBodyLimit::max(MAX_EMBED_BODY_BYTES)),
        );
    Router::new()
        .route(EMBEDDING_SERVICE_HEALTH_PATH, get(health))
        .merge(embed_routes)
        .with_state(ApiState { engine: queued })
}

fn overload_response(error: BoxError) -> Response {
    if error.is::<tower::load_shed::error::Overloaded>() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(EmbeddingServiceError {
                error: QueueFull.to_string(),
            }),
        )
            .into_response();
    }
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(EmbeddingServiceError {
            error: error.to_string(),
        }),
    )
        .into_response()
}

async fn health(State(state): State<ApiState>) -> Json<EmbeddingModelMetadata> {
    Json(state.engine.metadata().clone())
}

#[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
async fn embed(State(state): State<ApiState>, Json(request): Json<EmbeddingRequest>) -> Response {
    #[cfg(feature = "metrics")]
    metrics::counter!("chelix_embedding_service_requests_total").increment(1);

    match state.engine.embed(&request.text, request.priority).await {
        Ok(embedding) => Json(EmbeddingResponse { embedding }).into_response(),
        Err(error) => embed_error_response(error),
    }
}

fn embed_error_response(error: Error) -> Response {
    #[cfg(feature = "metrics")]
    metrics::counter!("chelix_embedding_service_errors_total").increment(1);

    if error.downcast_ref::<QueueFull>().is_some() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(EmbeddingServiceError {
                error: QueueFull.to_string(),
            }),
        )
            .into_response();
    }
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(EmbeddingServiceError {
            error: error.to_string(),
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use std::{
        net::Ipv4Addr,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use {
        anyhow::Result,
        async_trait::async_trait,
        chelix_protocol::{
            EMBEDDING_PRIORITY_INDEX, EMBEDDING_PRIORITY_SEARCH, EmbeddingRequest,
            EmbeddingResponse,
        },
        tokio::sync::{Mutex, oneshot},
    };

    use super::*;

    struct FakeEngine {
        metadata: EmbeddingModelMetadata,
        fail: bool,
    }

    #[async_trait]
    impl EmbeddingEngine for FakeEngine {
        async fn embed(&self, _text: &str, _priority: u32) -> Result<Vec<f32>> {
            if self.fail {
                anyhow::bail!("synthetic embedding failure");
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
                provider_key: "local-mistral:q8:test-model:0123456789abcdef".into(),
            },
            fail,
        })
    }

    async fn spawn_api(fail: bool) -> String {
        spawn_router(router(fake_engine(fail))).await
    }

    async fn spawn_router(router: Router) -> String {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap_or_else(|error| panic!("bind failed: {error}"));
        let address = listener
            .local_addr()
            .unwrap_or_else(|error| panic!("local address failed: {error}"));
        tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router).await {
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
                priority: 0,
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
                priority: 0,
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

    struct BlockingEngine {
        metadata: EmbeddingModelMetadata,
        started: AtomicUsize,
        gate: Mutex<Option<oneshot::Receiver<()>>>,
    }

    #[async_trait]
    impl EmbeddingEngine for BlockingEngine {
        async fn embed(&self, _text: &str, _priority: u32) -> Result<Vec<f32>> {
            self.started.fetch_add(1, Ordering::SeqCst);
            if let Some(gate) = self.gate.lock().await.take() {
                let _ = gate.await;
            }
            Ok(vec![1.0, 2.0, 3.0])
        }

        fn metadata(&self) -> &EmbeddingModelMetadata {
            &self.metadata
        }
    }

    #[tokio::test]
    async fn embed_rejects_overflow_before_first_job_finishes() {
        let (release_tx, release_rx) = oneshot::channel();
        let engine = Arc::new(BlockingEngine {
            metadata: EmbeddingModelMetadata {
                model_name: "test-model".into(),
                dimensions: 3,
                provider_key: "test".into(),
            },
            started: AtomicUsize::new(0),
            gate: Mutex::new(Some(release_rx)),
        });
        let base_url = spawn_router(router_with_limits(engine.clone(), 1, 2)).await;
        let client = reqwest::Client::new();
        let url = format!("{base_url}{EMBEDDING_SERVICE_EMBED_PATH}");

        let first = {
            let client = client.clone();
            let url = url.clone();
            tokio::spawn(async move {
                client
                    .post(url)
                    .json(&serde_json::json!({ "text": "first" }))
                    .send()
                    .await
            })
        };
        while engine.started.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }

        let waiting = {
            let client = client.clone();
            let url = url.clone();
            tokio::spawn(async move {
                client
                    .post(url)
                    .json(&serde_json::json!({ "text": "waiting" }))
                    .send()
                    .await
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let overflow = client
            .post(&url)
            .json(&serde_json::json!({ "text": "overflow" }))
            .send()
            .await
            .unwrap_or_else(|error| panic!("overflow request failed: {error}"));
        assert_eq!(overflow.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = overflow
            .json::<EmbeddingServiceError>()
            .await
            .unwrap_or_else(|error| panic!("overflow decode failed: {error}"));
        assert!(body.error.contains("embedding queue is full"));

        release_tx
            .send(())
            .unwrap_or_else(|_| panic!("release first job"));
        first
            .await
            .unwrap_or_else(|error| panic!("join first: {error}"))
            .unwrap_or_else(|error| panic!("first request: {error}"));
        waiting
            .await
            .unwrap_or_else(|error| panic!("join waiting: {error}"))
            .unwrap_or_else(|error| panic!("waiting request: {error}"));
    }

    struct RecordingEngine {
        metadata: EmbeddingModelMetadata,
        order: Mutex<Vec<String>>,
        started: AtomicUsize,
        gate: Mutex<Option<oneshot::Receiver<()>>>,
    }

    #[async_trait]
    impl EmbeddingEngine for RecordingEngine {
        async fn embed(&self, text: &str, _priority: u32) -> Result<Vec<f32>> {
            self.started.fetch_add(1, Ordering::SeqCst);
            if let Some(gate) = self.gate.lock().await.take() {
                let _ = gate.await;
            }
            self.order.lock().await.push(text.to_owned());
            Ok(vec![1.0, 2.0, 3.0])
        }

        fn metadata(&self) -> &EmbeddingModelMetadata {
            &self.metadata
        }
    }

    #[tokio::test]
    async fn http_router_runs_search_before_waiting_index() {
        let (release_tx, release_rx) = oneshot::channel();
        let engine = Arc::new(RecordingEngine {
            metadata: EmbeddingModelMetadata {
                model_name: "test-model".into(),
                dimensions: 3,
                provider_key: "test".into(),
            },
            order: Mutex::new(Vec::new()),
            started: AtomicUsize::new(0),
            gate: Mutex::new(Some(release_rx)),
        });
        let base_url = spawn_router(router_with_limits(engine.clone(), 8, 8)).await;
        let client = reqwest::Client::new();
        let url = format!("{base_url}{EMBEDDING_SERVICE_EMBED_PATH}");

        let first = {
            let client = client.clone();
            let url = url.clone();
            tokio::spawn(async move {
                client
                    .post(url)
                    .json(&EmbeddingRequest {
                        text: "first".into(),
                        priority: EMBEDDING_PRIORITY_INDEX,
                    })
                    .send()
                    .await
            })
        };
        while engine.started.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }

        let index = {
            let client = client.clone();
            let url = url.clone();
            tokio::spawn(async move {
                client
                    .post(url)
                    .json(&EmbeddingRequest {
                        text: "index".into(),
                        priority: EMBEDDING_PRIORITY_INDEX,
                    })
                    .send()
                    .await
            })
        };
        let search = {
            let client = client.clone();
            let url = url.clone();
            tokio::spawn(async move {
                client
                    .post(url)
                    .json(&EmbeddingRequest {
                        text: "search".into(),
                        priority: EMBEDDING_PRIORITY_SEARCH,
                    })
                    .send()
                    .await
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        release_tx
            .send(())
            .unwrap_or_else(|_| panic!("release first job"));
        first
            .await
            .unwrap_or_else(|error| panic!("join first: {error}"))
            .unwrap_or_else(|error| panic!("first request: {error}"));
        search
            .await
            .unwrap_or_else(|error| panic!("join search: {error}"))
            .unwrap_or_else(|error| panic!("search request: {error}"));
        index
            .await
            .unwrap_or_else(|error| panic!("join index: {error}"))
            .unwrap_or_else(|error| panic!("index request: {error}"));

        let order = engine.order.lock().await.clone();
        assert_eq!(order, vec!["first", "search", "index"]);
    }
}
