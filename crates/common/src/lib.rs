//! Shared types, error definitions, and utilities used across all chelix crates.

pub mod context_budget;
pub mod error;
pub mod hooks;
pub mod http_client;
pub mod model;
pub mod secret_serde;
pub mod ssrf;
pub mod tool_lifecycle;
pub mod types;

pub use {
    context_budget::ContextBudgetMetadata,
    error::{ChelixError, Error, FromMessage, Result},
    model::{
        ModelConfigMap, ModelMetadata, ModelMetadataError, ModelModality, ModelReasoningMetadata,
        PartialModelMetadata, PartialReasoningMetadata, ReasoningContent, ReasoningEffort,
        ReasoningInclude, ReasoningSummary, ResponsesReasoningItem,
    },
    tool_lifecycle::ActiveToolInvocation,
};
