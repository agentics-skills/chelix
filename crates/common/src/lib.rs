//! Shared types, error definitions, and utilities used across all chelix crates.

pub mod error;
pub mod hooks;
pub mod http_client;
pub mod model;
pub mod secret_serde;
pub mod ssrf;
pub mod types;

pub use {
    error::{ChelixError, Error, FromMessage, Result},
    model::{
        ModelConfigMap, ModelMetadata, ModelMetadataError, ModelModality, ModelReasoningMetadata,
        PartialModelMetadata, PartialReasoningMetadata, ReasoningEffort, ReasoningInclude,
        ReasoningSummary,
    },
};
