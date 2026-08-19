//! Shared types, error definitions, and utilities used across all chelix crates.

pub mod context_budget;
pub mod error;
pub mod hooks;
pub mod http_client;
pub mod item_positions;
pub mod model;
pub mod provider_output;
pub mod secret_serde;
pub mod ssrf;
pub mod tool_lifecycle;
pub mod types;

pub use {
    context_budget::ContextBudgetMetadata,
    error::{ChelixError, Error, FromMessage, Result},
    item_positions::ItemPositionAllocator,
    model::{
        ModelConfigMap, ModelMetadata, ModelMetadataError, ModelModality, ModelReasoningMetadata,
        PartialModelMetadata, PartialReasoningMetadata, ReasoningContent, ReasoningEffort,
        ReasoningInclude, ReasoningSummary, ResponsesReasoningItem,
    },
    provider_output::{
        MaterializerError, ProviderItemId, ProviderItemPosition, ProviderItemUpdate,
        ProviderItemUpdatePayload, ProviderOutputItem, ProviderOutputPayload, ProviderSegment,
        ProviderSegmentId, ProviderSegmentMaterializer, ProviderSegmentOutcome, ReasoningItem,
        ReasoningPart,
    },
    tool_lifecycle::ActiveToolInvocation,
};
