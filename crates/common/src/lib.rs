//! Shared types, error definitions, and utilities used across all chelix crates.

pub mod context_budget;
pub mod error;
pub mod hooks;
pub mod http_client;
pub mod item_positions;
pub mod message_medium;
pub mod model;
pub mod model_override;
pub mod provider_output;
pub mod reasoning_policy;
pub mod secret_serde;
pub mod ssrf;
pub mod tool_lifecycle;
pub mod tool_policy;
pub mod types;

pub use {
    context_budget::ContextBudgetMetadata,
    error::{ChelixError, Error, FromMessage, Result},
    item_positions::ItemPositionAllocator,
    message_medium::MessageMedium,
    model::{
        ModelConfigMap, ModelMetadata, ModelMetadataError, ModelModality, PartialModelMetadata,
        ReasoningContent, ReasoningEffort, ReasoningInclude, ReasoningSummary,
        ResolvedModelReasoning, ResolvedModelReasoningError, ResponsesReasoningItem,
    },
    model_override::{ConfigModelOverride, ModelOverride},
    provider_output::{
        MaterializerError, ProviderItemId, ProviderItemPosition, ProviderItemUpdate,
        ProviderItemUpdatePayload, ProviderOutputItem, ProviderOutputPayload, ProviderSegment,
        ProviderSegmentId, ProviderSegmentMaterializer, ProviderSegmentOutcome, ReasoningItem,
        ReasoningPart,
    },
    reasoning_policy::{ReasoningPolicyDecision, ReasoningRequestState, resolve_reasoning_policy},
    tool_lifecycle::ActiveToolInvocation,
    tool_policy::ToolPolicy,
};
