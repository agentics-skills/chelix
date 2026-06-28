/// Local GGUF embedding provider using llama-cpp-2.
///
/// Provides offline embedding via small GGUF models (e.g. EmbeddingGemma-300M).
/// Requires the `local-embeddings` feature flag and CMake + C++ compiler at build time.
///
/// The provider is intentionally model-agnostic: embedding dimensions and the
/// usable context size are read from the loaded model itself, so swapping the
/// GGUF file (or pointing `memory.model` at a different one) needs no code
/// changes here.
use std::{num::NonZeroU32, path::PathBuf};

use {
    anyhow::{Context, Result, bail},
    async_trait::async_trait,
    llama_cpp_2::{
        context::params::LlamaContextParams,
        llama_backend::LlamaBackend,
        llama_batch::LlamaBatch,
        model::{AddBos, LlamaModel, params::LlamaModelParams},
        token::LlamaToken,
    },
    tokio::sync::Mutex,
    tracing::{info, warn},
};

use crate::embeddings::EmbeddingProvider;

/// Default model used when no explicit model file is configured:
/// EmbeddingGemma-300M quantized to Q8_0 (~300MB). This is only a download
/// default — the actual dimensions and context limit are still derived from
/// whatever model file ends up being loaded.
const DEFAULT_MODEL_FILENAME: &str = "embeddinggemma-300M-Q8_0.gguf";
const DEFAULT_MODEL_URL: &str = "https://huggingface.co/ggml-org/embeddinggemma-300M-GGUF/resolve/main/embeddinggemma-300M-Q8_0.gguf";

/// Wrapper around `LlamaBackend` that opts into `Send + Sync`.
///
/// `LlamaBackend` is `!Send` because `llama-cpp-2` doesn't mark its FFI
/// handle as thread-safe. In practice the backend is an opaque init token
/// with no mutable state after construction, so sharing across threads is
/// safe. Wrapping it in a newtype keeps the `unsafe` declaration localised
/// rather than applying `unsafe impl` to the entire provider struct.
struct SendSyncBackend(LlamaBackend);

// SAFETY: LlamaBackend is an immutable init handle with no thread-local state.
unsafe impl Send for SendSyncBackend {}
unsafe impl Sync for SendSyncBackend {}

pub struct LocalGgufEmbeddingProvider {
    backend: SendSyncBackend,
    model: Mutex<LlamaModel>,
    /// Embedding dimensionality, read from the model (`n_embd`).
    dims: usize,
    /// Maximum number of tokens the model can encode in a single pass,
    /// read from the model (`n_ctx_train`). Longer inputs are chunked and
    /// mean-pooled instead of being truncated or crashing the process.
    max_tokens: usize,
    /// Number of CPU threads used for the embedding forward pass. Derived from
    /// the host's available parallelism so larger machines embed faster without
    /// any per-model tuning.
    n_threads: i32,
    /// Human-readable model name, derived from the model file stem.
    model_name: String,
    /// Stable cache key, derived from the model file so the embedding cache
    /// is invalidated automatically when the model changes.
    provider_key: String,
}

impl LocalGgufEmbeddingProvider {
    /// Load a GGUF model from a specific path.
    pub fn new(model_path: PathBuf) -> Result<Self> {
        let backend = LlamaBackend::init()?;
        let model_params = LlamaModelParams::default();
        let model = LlamaModel::load_from_file(&backend, &model_path, &model_params)
            .map_err(|e| anyhow::anyhow!("failed to load GGUF model: {e}"))?;

        // Derive dimensions and context limit from the model itself so the
        // provider works with any embedding GGUF without code changes.
        let dims = usize::try_from(model.n_embd())
            .map_err(|_| anyhow::anyhow!("model reported a non-positive embedding size"))?;
        if dims == 0 {
            bail!("model reported zero embedding dimensions");
        }
        let max_tokens = usize::try_from(model.n_ctx_train())
            .map_err(|_| anyhow::anyhow!("model context size does not fit into usize"))?;
        if max_tokens == 0 {
            bail!("model reported zero training context size");
        }

        let model_name = model_name_from_path(&model_path);
        let provider_key = provider_key_from_path(&model_path);
        let n_threads = available_thread_count();

        info!(
            path = %model_path.display(),
            model = %model_name,
            dims,
            max_tokens,
            n_threads,
            "loaded local GGUF embedding model"
        );

        Ok(Self {
            backend: SendSyncBackend(backend),
            model: Mutex::new(model),
            dims,
            max_tokens,
            n_threads,
            model_name,
            provider_key,
        })
    }

    /// Resolve which GGUF file to load.
    ///
    /// If `model` is set it is honored first: an existing absolute/relative file
    /// path is used as-is, otherwise it is treated as a filename inside
    /// `cache_dir`. When `model` is unset (or cannot be found) the bundled
    /// default model is ensured (downloaded if missing). This is what makes the
    /// provider model-agnostic — swapping models is a config change, not a code
    /// change.
    pub async fn resolve_model(cache_dir: PathBuf, model: Option<&str>) -> Result<PathBuf> {
        if let Some(model) = model.map(str::trim).filter(|m| !m.is_empty()) {
            let candidate = PathBuf::from(model);
            if candidate.is_file() {
                info!(path = %candidate.display(), "using configured local embedding model");
                return Ok(candidate);
            }
            let in_cache = cache_dir.join(model);
            if in_cache.is_file() {
                info!(
                    path = %in_cache.display(),
                    "using configured local embedding model from cache"
                );
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

    /// Default cache directory: `~/.moltis/models/`.
    pub fn default_cache_dir() -> PathBuf {
        directories::ProjectDirs::from("", "", "moltis")
            .map(|d: directories::ProjectDirs| d.data_dir().join("models"))
            .unwrap_or_else(|| PathBuf::from(".moltis/models"))
    }
}

/// Determine how many CPU threads to use for the embedding forward pass.
///
/// Uses the host's available parallelism and clamps to a positive `i32`.
/// Falls back to a single thread if the platform cannot report a value.
fn available_thread_count() -> i32 {
    let parallelism = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1);
    i32::try_from(parallelism).unwrap_or(i32::MAX).max(1)
}

/// Derive a human-readable model name from the GGUF file path.
fn model_name_from_path(path: &std::path::Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map_or_else(|| "local-gguf".to_string(), ToString::to_string)
}

/// Derive a stable cache key from the GGUF file name. Using the file name
/// keeps the embedding cache correct across model swaps without hardcoding a
/// per-model constant.
fn provider_key_from_path(path: &std::path::Path) -> String {
    let stem = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("local-gguf");
    format!("local-gguf:{stem}")
}

/// Encode a single window of tokens and return the pooled sequence embedding.
fn encode_window(
    backend: &LlamaBackend,
    model: &LlamaModel,
    window: &[LlamaToken],
    n_ctx: u32,
    n_threads: i32,
) -> Result<Vec<f32>> {
    let n_tokens = u32::try_from(window.len())
        .map_err(|_| anyhow::anyhow!("token window does not fit into u32"))?;

    // Use the model's full training context for `n_ctx` so we utilize the model
    // to capacity (and avoid llama.cpp's "n_ctx_seq < n_ctx_train" warning),
    // while sizing the (micro)batch to the actual window. Sizing `n_ubatch` to
    // the window keeps the compute buffer minimal and still satisfies the
    // encoder requirement `GGML_ASSERT(n_ubatch >= n_tokens)`. Decoder-style
    // models go through `decode`.
    let ctx_tokens = n_ctx.max(n_tokens);
    let ctx_params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(ctx_tokens))
        .with_n_batch(n_tokens)
        .with_n_ubatch(n_tokens)
        .with_n_threads(n_threads)
        .with_n_threads_batch(n_threads)
        .with_embeddings(true);

    let mut ctx = model
        .new_context(backend, ctx_params)
        .map_err(|e| anyhow::anyhow!("failed to create llama context: {e}"))?;

    let mut batch = LlamaBatch::new(window.len(), 1);
    for (i, &token) in window.iter().enumerate() {
        let pos = i32::try_from(i).map_err(|_| anyhow::anyhow!("token position overflow"))?;
        // Mark every token as an output so pooling has all hidden states and
        // llama.cpp does not need to override the request mid-encode.
        batch
            .add(token, pos, &[0], true)
            .map_err(|e| anyhow::anyhow!("batch add failed: {e}"))?;
    }

    // Encoder models expose embeddings via `encode`; decoder models via
    // `decode`. Try `encode` first and fall back so we stay model-agnostic.
    if let Err(enc_err) = ctx.encode(&mut batch) {
        ctx.decode(&mut batch).map_err(|dec_err| {
            anyhow::anyhow!("encode failed: {enc_err}; decode fallback failed: {dec_err}")
        })?;
    }

    let embeddings = ctx
        .embeddings_seq_ith(0)
        .map_err(|e| anyhow::anyhow!("get embeddings failed: {e}"))?;

    Ok(embeddings.to_vec())
}

/// Embed a text using the given model and backend. Must be called from a sync context.
///
/// `max_tokens` is the model's own context limit. Inputs longer than the limit
/// are split into non-overlapping windows and mean-pooled, so no input can ever
/// exceed the encoder batch and abort the process.
fn embed_sync(
    backend: &LlamaBackend,
    model: &LlamaModel,
    text: &str,
    max_tokens: usize,
    n_threads: i32,
) -> Result<Vec<f32>> {
    let tokens = model
        .str_to_token(text, AddBos::Always)
        .map_err(|e| anyhow::anyhow!("tokenization failed: {e}"))?;

    if tokens.is_empty() {
        bail!("empty token sequence");
    }

    debug_assert!(max_tokens > 0, "max_tokens must be positive");
    let window_size = max_tokens.max(1);

    // Full model context for `n_ctx`; saturates to u32 for very large limits.
    let n_ctx_full = u32::try_from(window_size).unwrap_or(u32::MAX);

    // Fast path: the whole input fits into one context window.
    if tokens.len() <= window_size {
        return encode_window(backend, model, &tokens, n_ctx_full, n_threads);
    }

    // Slow path: average the per-window embeddings. This keeps long documents
    // representable without truncation and without ever exceeding `n_ubatch`.
    let window_count = tokens.len().div_ceil(window_size);
    warn!(
        token_count = tokens.len(),
        max_tokens, window_count, "local embedding input exceeds model context; pooling windows"
    );

    let mut accumulator: Vec<f32> = Vec::new();
    for window in tokens.chunks(window_size) {
        let window_embedding = encode_window(backend, model, window, n_ctx_full, n_threads)?;
        if accumulator.is_empty() {
            accumulator = vec![0.0; window_embedding.len()];
        } else if accumulator.len() != window_embedding.len() {
            bail!("inconsistent embedding dimensions across windows");
        }
        for (acc, value) in accumulator.iter_mut().zip(window_embedding) {
            *acc += value;
        }
    }

    let divisor = window_count as f32;
    for value in &mut accumulator {
        *value /= divisor;
    }

    Ok(accumulator)
}

#[async_trait]
impl EmbeddingProvider for LocalGgufEmbeddingProvider {
    async fn embed(&self, text: &str) -> crate::error::Result<Vec<f32>> {
        let model = self.model.lock().await;
        let text = text.to_string();
        let max_tokens = self.max_tokens;
        let n_threads = self.n_threads;
        // llama-cpp-2 is CPU-bound; use block_in_place to avoid starving the async runtime
        let backend = &self.backend.0;
        let model_ref = &*model;
        let result = tokio::task::block_in_place(move || {
            embed_sync(backend, model_ref, &text, max_tokens, n_threads)
        })
        .map_err(|e| crate::error::Error::Embedding(e.to_string()))?;
        Ok(result)
    }

    fn model_name(&self) -> &str {
        &self.model_name
    }

    fn dimensions(&self) -> usize {
        self.dims
    }

    fn provider_key(&self) -> &str {
        &self.provider_key
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_cache_dir() {
        let dir = LocalGgufEmbeddingProvider::default_cache_dir();
        assert!(dir.to_string_lossy().contains("models"));
    }

    #[test]
    fn model_name_uses_file_stem() {
        let path = std::path::Path::new("/models/embeddinggemma-300M-Q8_0.gguf");
        assert_eq!(model_name_from_path(path), "embeddinggemma-300M-Q8_0");
    }

    #[test]
    fn provider_key_changes_with_model_file() {
        let a = provider_key_from_path(std::path::Path::new("/models/model-a.gguf"));
        let b = provider_key_from_path(std::path::Path::new("/models/model-b.gguf"));
        assert_ne!(
            a, b,
            "different model files must yield different cache keys"
        );
        assert!(a.starts_with("local-gguf:"));
    }
}
