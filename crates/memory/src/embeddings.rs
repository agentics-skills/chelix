/// Provider-agnostic embedding trait for generating vectors from text.
use async_trait::async_trait;

use crate::error::Result;

#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Generate an embedding for a single text.
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;

    /// Generate embeddings for a batch of texts.
    /// Default implementation calls `embed` sequentially.
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut results = Vec::with_capacity(texts.len());
        for text in texts {
            results.push(self.embed(text).await?);
        }
        Ok(results)
    }

    /// Generate an embedding, forwarding `priority` to providers that queue work.
    /// Default implementation ignores priority and calls [`Self::embed`].
    async fn embed_with_priority(&self, text: &str, _priority: u32) -> Result<Vec<f32>> {
        self.embed(text).await
    }

    /// Sidecar JSON body limit when this provider talks to the local embed service.
    fn max_embed_payload_bytes(&self) -> Option<usize> {
        None
    }

    /// The model name used by this provider (e.g. "text-embedding-3-small").
    fn model_name(&self) -> &str;

    /// The dimensionality of the embeddings produced.
    fn dimensions(&self) -> usize;

    /// A stable key identifying this provider configuration for cache discrimination.
    /// Different providers or the same provider with different settings should return
    /// different keys.
    fn provider_key(&self) -> &str;
}
