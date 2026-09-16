//! HTTP API surface for the managed local embedding service.

pub mod api;
mod engine_api;
pub mod pool;
pub mod queue;

pub use engine_api::EmbeddingEngine;
