//! Semantic history identities, snapshots and range requests.

use std::collections::BTreeMap;

use {
    chelix_common::{ProviderSegmentId, ProviderSegmentMaterializer, ProviderSegmentOutcome},
    serde::{Deserialize, Serialize},
    serde_json::Value,
};

use crate::{Error, PersistedMessage, Result};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UiMessageId(pub String);

impl UiMessageId {
    #[must_use]
    pub fn segment(id: &ProviderSegmentId) -> Self {
        Self(format!("segment:{}", id.as_str()))
    }

    #[must_use]
    pub fn tool(run_id: &str, call_id: &str) -> Self {
        Self(format!("tool:{}:{run_id}:{call_id}", run_id.len()))
    }
}

pub fn validate_client_message_id(id: &str) -> Result<()> {
    let uuid = uuid::Uuid::parse_str(id)
        .map_err(|error| Error::message(format!("invalid clientMessageId: {error}")))?;
    if uuid.to_string() != id {
        return Err(Error::message("clientMessageId must be a canonical UUID"));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UiGeneration(pub String);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiHistoryTarget {
    pub message_id: UiMessageId,
    pub generation: UiGeneration,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "direction", rename_all = "snake_case", deny_unknown_fields)]
pub enum UiHistoryRange {
    #[default]
    Latest,
    Before {
        position: u64,
    },
    After {
        position: u64,
    },
    Around {
        message_id: UiMessageId,
    },
    Window {
        start: u64,
        end: Option<u64>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiHistoryPage {
    pub generation: UiGeneration,
    pub revision: u64,
    pub total_messages: u32,
    pub history: Vec<UiSnapshot>,
    pub has_older: bool,
    pub has_newer: bool,
    pub first_position: Option<u64>,
    pub last_position: Option<u64>,
}

impl UiHistoryPage {
    pub fn public_value(&self) -> Result<Value> {
        let mut value = serde_json::to_value(self)?;
        crate::redact_backend_only_provider_state(&mut value);
        Ok(value)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiHistoryRevision {
    pub session_key: String,
    pub generation: UiGeneration,
    pub revision: u64,
    pub total_messages: u32,
    pub failure: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiHistoryBatch {
    pub generation: UiGeneration,
    pub from_revision: u64,
    pub revision: u64,
    pub total_messages: u32,
    pub history: Vec<UiSnapshot>,
}

impl UiHistoryBatch {
    pub fn public_value(&self) -> Result<Value> {
        let mut value = serde_json::to_value(self)?;
        crate::redact_backend_only_provider_state(&mut value);
        Ok(value)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiPresentation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document: Option<UiPresentationDocument>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "format", content = "content", rename_all = "snake_case")]
pub enum UiPresentationDocument {
    Text(String),
    Markdown(String),
    Diff(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiRecord {
    #[serde(flatten)]
    pub message: PersistedMessage,
    #[serde(rename = "clientMessageId", skip_serializing_if = "Option::is_none")]
    pub client_message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tts_provider: Option<String>,
}

impl From<PersistedMessage> for UiRecord {
    fn from(message: PersistedMessage) -> Self {
        Self {
            message,
            client_message_id: None,
            tts_provider: None,
        }
    }
}

impl TryFrom<Value> for UiRecord {
    type Error = Error;

    fn try_from(value: Value) -> Result<Self> {
        Ok(serde_json::from_value(value)?)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum UiErrorMessage {
    Error { error: UiProviderError },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiProviderError {
    pub run_id: String,
    pub segment_id: Option<ProviderSegmentId>,
    pub created_at: u64,
    pub raw: String,
    pub details: Value,
    pub retry_after_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UiContent {
    Record(Box<UiRecord>),
    Error(UiErrorMessage),
}

impl UiContent {
    #[must_use]
    pub fn run_id(&self) -> Option<&str> {
        match self {
            Self::Record(record) => record.message.run_id(),
            Self::Error(UiErrorMessage::Error { error }) => Some(&error.run_id),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiSnapshot {
    pub id: UiMessageId,
    pub position: u64,
    pub revision: u64,
    pub canonical_committed: bool,
    #[serde(
        flatten,
        serialize_with = "crate::ui_history_serialization::serialize_content"
    )]
    pub content: UiContent,
    pub presentation: UiPresentation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accumulated_arguments: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assistant_id: Option<UiMessageId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ProviderSegmentOutcome>,
}

impl UiSnapshot {
    pub fn public_value(&self) -> Result<Value> {
        let mut value = serde_json::to_value(self)?;
        crate::redact_backend_only_provider_state(&mut value);
        Ok(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct UiCanonicalBinding {
    pub start: usize,
    pub end: usize,
    pub record_index: Option<usize>,
}

#[derive(Debug, Clone)]
pub(crate) struct UiRecordUpdate {
    pub generation: UiGeneration,
    pub entry: UiEntry,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct UiEntry {
    pub snapshot: UiSnapshot,
    pub canonical: Option<UiCanonicalBinding>,
    pub content_version: u64,
    pub materializer: Option<ProviderSegmentMaterializer>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiSearchHit {
    pub session_key: String,
    pub generation: UiGeneration,
    pub message_id: UiMessageId,
    pub position: u64,
    pub snippet: String,
    pub role: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiForkBoundaryReason {
    ActiveContent,
    InterleavedSegment,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiForkResult {
    pub fork_point: u32,
    pub source_end: u64,
    pub boundary_adjusted: bool,
    pub boundary_reasons: Vec<UiForkBoundaryReason>,
}

#[derive(Debug, Clone)]
pub struct UiRunMetadata {
    pub run_id: String,
    pub model: String,
    pub provider: String,
    pub reasoning_effort: Option<String>,
}
