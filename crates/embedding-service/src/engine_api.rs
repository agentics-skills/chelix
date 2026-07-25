use {anyhow::Result, async_trait::async_trait, chelix_protocol::EmbeddingModelMetadata};

#[async_trait]
pub trait EmbeddingEngine: Send + Sync {
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;
    fn metadata(&self) -> &EmbeddingModelMetadata;
}
