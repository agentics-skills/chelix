use std::{num::NonZeroU32, path::PathBuf};

use {
    anyhow::{Result, bail},
    async_trait::async_trait,
    chelix_embedding_service::EmbeddingEngine,
    chelix_protocol::EmbeddingModelMetadata,
    llama_cpp_2::{
        context::params::LlamaContextParams,
        llama_backend::LlamaBackend,
        llama_batch::LlamaBatch,
        model::{AddBos, LlamaModel, params::LlamaModelParams},
        token::LlamaToken,
    },
    tokio::sync::Mutex,
};

struct SendSyncBackend(LlamaBackend);

// SAFETY: LlamaBackend is an immutable init handle with no thread-local state.
unsafe impl Send for SendSyncBackend {}
// SAFETY: LlamaBackend is an immutable init handle with no thread-local state.
unsafe impl Sync for SendSyncBackend {}

pub(crate) struct LocalGgufEngine {
    backend: SendSyncBackend,
    model: Mutex<LlamaModel>,
    metadata: EmbeddingModelMetadata,
    max_tokens: usize,
    n_threads: i32,
}

impl LocalGgufEngine {
    pub(crate) fn new(model_path: PathBuf) -> Result<Self> {
        let backend = LlamaBackend::init()?;
        let model = LlamaModel::load_from_file(&backend, &model_path, &LlamaModelParams::default())
            .map_err(|error| anyhow::anyhow!("failed to load GGUF model: {error}"))?;
        let dimensions = usize::try_from(model.n_embd())
            .map_err(|_| anyhow::anyhow!("model reported a non-positive embedding size"))?;
        if dimensions == 0 {
            bail!("model reported zero embedding dimensions");
        }
        let max_tokens = usize::try_from(model.n_ctx_train())
            .map_err(|_| anyhow::anyhow!("model context size does not fit into usize"))?;
        if max_tokens == 0 {
            bail!("model reported zero training context size");
        }
        let metadata = EmbeddingModelMetadata {
            model_name: model_name_from_path(&model_path),
            dimensions,
            provider_key: provider_key_from_path(&model_path),
        };
        let n_threads = available_thread_count();

        #[cfg(feature = "tracing")]
        tracing::info!(
            path = %model_path.display(),
            model = %metadata.model_name,
            dimensions,
            max_tokens,
            n_threads,
            "loaded local GGUF embedding model"
        );

        Ok(Self {
            backend: SendSyncBackend(backend),
            model: Mutex::new(model),
            metadata,
            max_tokens,
            n_threads,
        })
    }
}

#[async_trait]
impl EmbeddingEngine for LocalGgufEngine {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let model = self.model.lock().await;
        let text = text.to_string();
        let backend = &self.backend.0;
        let model_ref = &*model;
        let max_tokens = self.max_tokens;
        let n_threads = self.n_threads;
        tokio::task::block_in_place(move || {
            embed_sync(backend, model_ref, &text, max_tokens, n_threads)
        })
    }

    fn metadata(&self) -> &EmbeddingModelMetadata {
        &self.metadata
    }
}

fn available_thread_count() -> i32 {
    let parallelism = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1);
    i32::try_from(parallelism).unwrap_or(i32::MAX).max(1)
}

fn model_name_from_path(path: &std::path::Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map_or_else(|| "local-gguf".to_string(), ToString::to_string)
}

fn provider_key_from_path(path: &std::path::Path) -> String {
    let filename = path
        .file_name()
        .and_then(|filename| filename.to_str())
        .unwrap_or("local-gguf");
    format!("local-gguf:{filename}")
}

fn encode_window(
    backend: &LlamaBackend,
    model: &LlamaModel,
    window: &[LlamaToken],
    n_ctx: u32,
    n_threads: i32,
) -> Result<Vec<f32>> {
    let n_tokens = u32::try_from(window.len())
        .map_err(|_| anyhow::anyhow!("token window does not fit into u32"))?;
    let ctx_tokens = n_ctx.max(n_tokens);
    let ctx_params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(ctx_tokens))
        .with_n_batch(n_tokens)
        .with_n_ubatch(n_tokens)
        .with_n_threads(n_threads)
        .with_n_threads_batch(n_threads)
        .with_embeddings(true);
    let mut context = model
        .new_context(backend, ctx_params)
        .map_err(|error| anyhow::anyhow!("failed to create llama context: {error}"))?;
    let mut batch = LlamaBatch::new(window.len(), 1);
    for (index, &token) in window.iter().enumerate() {
        let position =
            i32::try_from(index).map_err(|_| anyhow::anyhow!("token position overflow"))?;
        batch
            .add(token, position, &[0], true)
            .map_err(|error| anyhow::anyhow!("batch add failed: {error}"))?;
    }
    if let Err(encode_error) = context.encode(&mut batch) {
        context.decode(&mut batch).map_err(|decode_error| {
            anyhow::anyhow!("encode failed: {encode_error}; decode fallback failed: {decode_error}")
        })?;
    }
    let embeddings = context
        .embeddings_seq_ith(0)
        .map_err(|error| anyhow::anyhow!("get embeddings failed: {error}"))?;
    Ok(embeddings.to_vec())
}

fn embed_sync(
    backend: &LlamaBackend,
    model: &LlamaModel,
    text: &str,
    max_tokens: usize,
    n_threads: i32,
) -> Result<Vec<f32>> {
    let tokens = model
        .str_to_token(text, AddBos::Always)
        .map_err(|error| anyhow::anyhow!("tokenization failed: {error}"))?;
    if tokens.is_empty() {
        bail!("empty token sequence");
    }

    debug_assert!(max_tokens > 0, "max_tokens must be positive");
    let window_size = max_tokens.max(1);
    let n_ctx_full = u32::try_from(window_size).unwrap_or(u32::MAX);
    if tokens.len() <= window_size {
        return encode_window(backend, model, &tokens, n_ctx_full, n_threads);
    }

    let window_count = tokens.len().div_ceil(window_size);
    #[cfg(feature = "tracing")]
    tracing::warn!(
        token_count = tokens.len(),
        max_tokens,
        window_count,
        "local embedding input exceeds model context; pooling windows"
    );
    let mut accumulator: Vec<f32> = Vec::new();
    for window in tokens.chunks(window_size) {
        let window_embedding = encode_window(backend, model, window, n_ctx_full, n_threads)?;
        if accumulator.is_empty() {
            accumulator = vec![0.0; window_embedding.len()];
        } else if accumulator.len() != window_embedding.len() {
            bail!("inconsistent embedding dimensions across windows");
        }
        for (accumulated, value) in accumulator.iter_mut().zip(window_embedding) {
            *accumulated += value;
        }
    }
    let divisor = window_count as f32;
    for value in &mut accumulator {
        *value /= divisor;
    }
    Ok(accumulator)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_name_uses_file_stem() {
        let path = std::path::Path::new("/models/embeddinggemma-300M-Q8_0.gguf");
        assert_eq!(model_name_from_path(path), "embeddinggemma-300M-Q8_0");
    }

    #[test]
    fn provider_key_changes_with_model_file() {
        let first = provider_key_from_path(std::path::Path::new("/models/model-a.gguf"));
        let second = provider_key_from_path(std::path::Path::new("/models/model-b.gguf"));
        assert_ne!(first, second);
        assert!(first.starts_with("local-gguf:"));
    }

    #[test]
    fn thread_count_is_positive() {
        assert!(available_thread_count() > 0);
    }
}
