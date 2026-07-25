//! Managed client for the local GGUF embedding sidecar.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Mutex,
    time::Duration,
};

use {
    anyhow::{Context, Result, bail},
    async_trait::async_trait,
    chelix_protocol::{
        EMBEDDING_SERVICE_EMBED_PATH, EMBEDDING_SERVICE_PROTOCOL_VERSION, EmbeddingModelMetadata,
        EmbeddingRequest, EmbeddingResponse, EmbeddingServiceError, EmbeddingServiceReady,
    },
    tokio::{
        io::{AsyncBufReadExt, BufReader},
        process::{Child, Command},
    },
    tracing::{info, warn},
};

use crate::embeddings::EmbeddingProvider;

const DEFAULT_MODEL_FILENAME: &str = "embeddinggemma-300M-Q8_0.gguf";
const DEFAULT_MODEL_URL: &str = "https://huggingface.co/ggml-org/embeddinggemma-300M-GGUF/resolve/main/embeddinggemma-300M-Q8_0.gguf";
const SERVICE_BINARY_NAME: &str = "chelix-embedding-service";

struct ManagedService {
    child: Mutex<Child>,
}

impl ManagedService {
    fn new(child: Child) -> Self {
        Self {
            child: Mutex::new(child),
        }
    }
}

impl Drop for ManagedService {
    fn drop(&mut self) {
        let child = self
            .child
            .get_mut()
            .unwrap_or_else(|error| error.into_inner());
        if let Err(error) = child.start_kill() {
            warn!(%error, "failed to stop local embedding service");
        }
    }
}

pub struct LocalGgufEmbeddingProvider {
    client: reqwest::Client,
    embed_url: String,
    model: EmbeddingModelMetadata,
    _service: Option<ManagedService>,
}

impl LocalGgufEmbeddingProvider {
    /// Start the sidecar and load a GGUF model from a specific path.
    pub async fn new(model_path: PathBuf) -> Result<Self> {
        let service_path = embedding_service_binary()?;
        let mut command = Command::new(&service_path);
        command
            .arg("--model")
            .arg(&model_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);

        let mut child = command.spawn().with_context(|| {
            format!(
                "starting local embedding service at {}; build it separately with `cargo build -p chelix-embedding-service`",
                service_path.display()
            )
        })?;
        let stdout = child
            .stdout
            .take()
            .context("local embedding service stdout was not piped")?;
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();

        let bytes_read = match reader.read_line(&mut line).await {
            Ok(bytes_read) => bytes_read,
            Err(error) => {
                stop_child(&mut child).await;
                return Err(error).context("reading local embedding service startup message");
            },
        };
        if bytes_read == 0 {
            let status = child
                .wait()
                .await
                .context("waiting for local embedding service")?;
            bail!("local embedding service exited before startup: {status}");
        }

        let ready: EmbeddingServiceReady = match serde_json::from_str(line.trim()) {
            Ok(ready) => ready,
            Err(error) => {
                stop_child(&mut child).await;
                return Err(error).context("decoding local embedding service startup message");
            },
        };
        if let Err(error) = validate_ready(&ready) {
            stop_child(&mut child).await;
            return Err(error);
        }

        let base_url = format!("http://127.0.0.1:{}", ready.port);
        let provider =
            Self::from_endpoint(base_url, ready.model, Some(ManagedService::new(child)))?;
        info!(
            model = %provider.model.model_name,
            dimensions = provider.model.dimensions,
            "started managed local GGUF embedding service"
        );
        Ok(provider)
    }

    fn from_endpoint(
        base_url: String,
        model: EmbeddingModelMetadata,
        service: Option<ManagedService>,
    ) -> Result<Self> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            .build()
            .context("building local embedding HTTP client")?;
        Ok(Self {
            client,
            embed_url: format!("{base_url}{EMBEDDING_SERVICE_EMBED_PATH}"),
            model,
            _service: service,
        })
    }

    /// Resolve which GGUF file to load.
    pub async fn resolve_model(cache_dir: PathBuf, model: Option<&str>) -> Result<PathBuf> {
        if let Some(model) = model.map(str::trim).filter(|model| !model.is_empty()) {
            let candidate = PathBuf::from(model);
            if candidate.is_file() {
                info!(path = %candidate.display(), "using configured local embedding model");
                return Ok(candidate);
            }
            let in_cache = cache_dir.join(model);
            if in_cache.is_file() {
                info!(path = %in_cache.display(), "using configured local embedding model from cache");
                return Ok(in_cache);
            }
            warn!(
                model,
                "configured local embedding model not found; falling back to default"
            );
        }
        Self::ensure_model(cache_dir).await
    }

    /// Ensure the default model exists in the cache directory, downloading if needed.
    pub async fn ensure_model(cache_dir: PathBuf) -> Result<PathBuf> {
        let model_path = cache_dir.join(DEFAULT_MODEL_FILENAME);
        if model_path.exists() {
            info!(path = %model_path.display(), "local embedding model found in cache");
            return Ok(model_path);
        }

        tokio::fs::create_dir_all(&cache_dir)
            .await
            .context("creating model cache dir")?;
        info!(url = DEFAULT_MODEL_URL, "downloading local embedding model");

        let response = reqwest::get(DEFAULT_MODEL_URL)
            .await
            .context("downloading GGUF model")?
            .error_for_status()
            .context("GGUF model download failed")?;
        let bytes = response.bytes().await.context("reading model bytes")?;
        let tmp_path = model_path.with_extension("tmp");
        tokio::fs::write(&tmp_path, &bytes)
            .await
            .context("writing model file")?;
        tokio::fs::rename(&tmp_path, &model_path)
            .await
            .context("renaming model file")?;

        info!(
            path = %model_path.display(),
            size_mb = bytes.len() / (1024 * 1024),
            "local embedding model downloaded"
        );
        Ok(model_path)
    }

    /// Default cache directory: `~/.chelix/models/`.
    pub fn default_cache_dir() -> PathBuf {
        directories::ProjectDirs::from("", "", "chelix")
            .map(|dirs: directories::ProjectDirs| dirs.data_dir().join("models"))
            .unwrap_or_else(|| PathBuf::from(".chelix/models"))
    }
}

fn validate_ready(ready: &EmbeddingServiceReady) -> Result<()> {
    if ready.protocol_version != EMBEDDING_SERVICE_PROTOCOL_VERSION {
        bail!(
            "unsupported local embedding service protocol version {}; expected {}",
            ready.protocol_version,
            EMBEDDING_SERVICE_PROTOCOL_VERSION
        );
    }
    if ready.port == 0 {
        bail!("local embedding service reported an invalid port");
    }
    if ready.model.dimensions == 0 {
        bail!("local embedding service reported zero embedding dimensions");
    }
    if ready.model.model_name.is_empty() || ready.model.provider_key.is_empty() {
        bail!("local embedding service reported incomplete model metadata");
    }
    Ok(())
}

async fn stop_child(child: &mut Child) {
    if let Err(error) = child.kill().await {
        warn!(%error, "failed to stop local embedding service after startup error");
    }
}

fn embedding_service_binary() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("CHELIX_EMBEDDING_SERVICE") {
        return Ok(PathBuf::from(path));
    }

    let current_exe = std::env::current_exe().context("resolving current executable")?;
    let sibling = sibling_service_binary(&current_exe);
    if sibling.is_file() {
        return Ok(sibling);
    }
    Ok(PathBuf::from(service_binary_filename()))
}

fn sibling_service_binary(current_exe: &Path) -> PathBuf {
    current_exe.with_file_name(service_binary_filename())
}

fn service_binary_filename() -> OsString {
    let mut name = OsString::from(SERVICE_BINARY_NAME);
    if !std::env::consts::EXE_EXTENSION.is_empty() {
        name.push(".");
        name.push(std::env::consts::EXE_EXTENSION);
    }
    name
}

#[async_trait]
impl EmbeddingProvider for LocalGgufEmbeddingProvider {
    async fn embed(&self, text: &str) -> crate::error::Result<Vec<f32>> {
        let response = self
            .client
            .post(&self.embed_url)
            .json(&EmbeddingRequest {
                text: text.to_owned(),
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
                "local embedding service returned {status}: {error}"
            )));
        }
        let response = response.json::<EmbeddingResponse>().await?;
        if response.embedding.len() != self.model.dimensions {
            return Err(crate::error::Error::Embedding(format!(
                "local embedding service returned {} dimensions; expected {}",
                response.embedding.len(),
                self.model.dimensions
            )));
        }
        Ok(response.embedding)
    }

    fn model_name(&self) -> &str {
        &self.model.model_name
    }

    fn dimensions(&self) -> usize {
        self.model.dimensions
    }

    fn provider_key(&self) -> &str {
        &self.model.provider_key
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata() -> EmbeddingModelMetadata {
        EmbeddingModelMetadata {
            model_name: "test-model".into(),
            dimensions: 3,
            provider_key: "local-gguf:test-model.gguf".into(),
        }
    }

    #[test]
    fn default_cache_dir_contains_models() {
        let dir = LocalGgufEmbeddingProvider::default_cache_dir();
        assert!(dir.to_string_lossy().contains("models"));
    }

    #[test]
    fn sibling_binary_stays_next_to_current_executable() {
        let current = Path::new("/opt/chelix/bin/chelix");
        assert_eq!(
            sibling_service_binary(current),
            Path::new("/opt/chelix/bin").join(service_binary_filename())
        );
    }

    #[test]
    fn readiness_validation_rejects_protocol_mismatch() {
        let ready = EmbeddingServiceReady {
            protocol_version: EMBEDDING_SERVICE_PROTOCOL_VERSION + 1,
            port: 12_345,
            model: metadata(),
        };
        assert!(validate_ready(&ready).is_err());
    }

    #[tokio::test]
    async fn embed_uses_unauthenticated_loopback_api() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", EMBEDDING_SERVICE_EMBED_PATH)
            .match_body(mockito::Matcher::JsonString(
                serde_json::json!({ "text": "hello" }).to_string(),
            ))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"embedding":[1.0,2.0,3.0]}"#)
            .create_async()
            .await;
        let provider = LocalGgufEmbeddingProvider::from_endpoint(server.url(), metadata(), None)
            .unwrap_or_else(|error| panic!("provider creation failed: {error}"));

        let embedding = provider
            .embed("hello")
            .await
            .unwrap_or_else(|error| panic!("embedding failed: {error}"));

        assert_eq!(embedding, vec![1.0, 2.0, 3.0]);
        assert_eq!(provider.model_name(), "test-model");
        assert_eq!(provider.provider_key(), "local-gguf:test-model.gguf");
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
        let provider = LocalGgufEmbeddingProvider::from_endpoint(server.url(), metadata(), None)
            .unwrap_or_else(|error| panic!("provider creation failed: {error}"));

        let result = provider.embed("hello").await;

        assert!(result.is_err());
        assert!(
            result
                .err()
                .is_some_and(|error| error.to_string().contains("returned 1 dimensions"))
        );
    }
}
