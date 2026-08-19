//! Canonical provider segment and output item model.
//!
//! Enforces identity, position, and ordering preservation across streaming,
//! persistence, reload, broadcast, and request replay without loss or mutation.

use {
    crate::ReasoningContent,
    serde::{Deserialize, Serialize},
    std::collections::HashMap,
};

/// Unique identifier for a provider response/attempt segment.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderSegmentId(pub String);

impl ProviderSegmentId {
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ProviderSegmentId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for ProviderSegmentId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl std::fmt::Display for ProviderSegmentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Terminal or active outcome of a provider segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderSegmentOutcome {
    Active,
    Completed,
    Incomplete,
    Failed,
    Cancelled,
    TransportError,
}

/// Canonical identifier for a single item within a provider response.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderItemId(pub String);

impl ProviderItemId {
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ProviderItemId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for ProviderItemId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl std::fmt::Display for ProviderItemId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Canonical 0-based position of an item in the provider response output array.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderItemPosition(pub usize);

impl ProviderItemPosition {
    #[must_use]
    pub const fn new(position: usize) -> Self {
        Self(position)
    }

    #[must_use]
    pub const fn as_usize(self) -> usize {
        self.0
    }
}

impl From<usize> for ProviderItemPosition {
    fn from(pos: usize) -> Self {
        Self(pos)
    }
}

impl std::fmt::Display for ProviderItemPosition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// One summary/content part of a structured reasoning item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningPart {
    pub part_index: usize,
    pub text: String,
}

/// Reasoning payload for a provider item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningItem {
    pub id: ProviderItemId,
    pub output_index: ProviderItemPosition,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub summary_parts: Vec<ReasoningPart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted_content: Option<String>,
}

impl ReasoningItem {
    /// Return a public copy of the reasoning item with backend replay state removed.
    #[must_use]
    pub fn redacted(&self) -> Self {
        Self {
            id: self.id.clone(),
            output_index: self.output_index,
            summary_parts: self.summary_parts.clone(),
            visible_text: self.visible_text.clone(),
            encrypted_content: None,
        }
    }

    /// Extract visible display text/parts representation.
    #[must_use]
    pub fn to_reasoning_content(&self) -> Option<ReasoningContent> {
        if !self.summary_parts.is_empty() {
            let mut parts = Vec::with_capacity(self.summary_parts.len());
            for part in &self.summary_parts {
                if !part.text.is_empty() {
                    parts.push(part.text.clone());
                }
            }
            if parts.is_empty() {
                None
            } else {
                Some(ReasoningContent::Parts(parts))
            }
        } else if let Some(ref text) = self.visible_text {
            if text.is_empty() {
                None
            } else {
                Some(ReasoningContent::Text(text.clone()))
            }
        } else {
            None
        }
    }
}

/// Typed payload inside a provider output item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProviderOutputPayload {
    Reasoning(ReasoningItem),
    Message {
        text: String,
    },
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
    },
}

/// A single typed item in a provider's output envelope with its canonical identity and position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderOutputItem {
    pub id: ProviderItemId,
    pub position: ProviderItemPosition,
    pub payload: ProviderOutputPayload,
}

impl ProviderOutputItem {
    /// Return a redacted version safe for public UI transmission.
    #[must_use]
    pub fn redacted(&self) -> Self {
        Self {
            id: self.id.clone(),
            position: self.position,
            payload: match &self.payload {
                ProviderOutputPayload::Reasoning(r) => {
                    ProviderOutputPayload::Reasoning(r.redacted())
                },
                ProviderOutputPayload::Message { text } => {
                    ProviderOutputPayload::Message { text: text.clone() }
                },
                ProviderOutputPayload::FunctionCall {
                    call_id,
                    name,
                    arguments,
                } => ProviderOutputPayload::FunctionCall {
                    call_id: call_id.clone(),
                    name: name.clone(),
                    arguments: arguments.clone(),
                },
            },
        }
    }
}

/// Typed payload for an incremental stream update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "update_type", rename_all = "snake_case")]
pub enum ProviderItemUpdatePayload {
    ReasoningDelta {
        part_index: usize,
        delta: String,
    },
    ReasoningPartDone {
        part_index: usize,
        text: String,
    },
    ReasoningItemDone {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_content: Option<String>,
    },
    ReasoningText {
        text: String,
    },
    ReasoningTextDelta {
        delta: String,
    },
    MessageDelta {
        delta: String,
    },
    MessageDone {
        text: String,
    },
    FunctionCallStart {
        name: String,
    },
    FunctionCallDelta {
        delta: String,
    },
    FunctionCallDone {
        arguments: String,
    },
}

/// An append-only stream update addressed to a specific provider item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderItemUpdate {
    pub segment_id: ProviderSegmentId,
    pub item_id: ProviderItemId,
    pub position: ProviderItemPosition,
    pub update_seq: u64,
    pub payload: ProviderItemUpdatePayload,
}

impl ProviderItemUpdate {
    /// Return a redacted copy of the update safe for public transmission.
    #[must_use]
    pub fn redacted(&self) -> Self {
        Self {
            segment_id: self.segment_id.clone(),
            item_id: self.item_id.clone(),
            position: self.position,
            update_seq: self.update_seq,
            payload: match &self.payload {
                ProviderItemUpdatePayload::ReasoningItemDone { .. } => {
                    ProviderItemUpdatePayload::ReasoningItemDone {
                        encrypted_content: None,
                    }
                },
                other => other.clone(),
            },
        }
    }
}

/// Materialized provider segment representing a full response/attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSegment {
    /// Canonical segment identity as reported by the provider. `None` until the
    /// provider announces the segment, so the runtime never invents an identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segment_id: Option<ProviderSegmentId>,
    pub outcome: ProviderSegmentOutcome,
    pub items: Vec<ProviderOutputItem>,
}

impl ProviderSegment {
    #[must_use]
    pub fn new(segment_id: ProviderSegmentId) -> Self {
        Self {
            segment_id: Some(segment_id),
            outcome: ProviderSegmentOutcome::Active,
            items: Vec::new(),
        }
    }

    /// Create a segment whose identity is still owned by the provider.
    #[must_use]
    pub fn pending() -> Self {
        Self {
            segment_id: None,
            outcome: ProviderSegmentOutcome::Active,
            items: Vec::new(),
        }
    }

    /// Extract combined reasoning content from all reasoning items in output order.
    #[must_use]
    pub fn reasoning_content(&self) -> Option<ReasoningContent> {
        let mut all_parts = Vec::new();
        let mut single_text = String::new();
        let mut has_parts = false;
        let mut has_text = false;

        for item in &self.items {
            if let ProviderOutputPayload::Reasoning(ref reasoning) = item.payload {
                if !reasoning.summary_parts.is_empty() {
                    has_parts = true;
                    for part in &reasoning.summary_parts {
                        all_parts.push(part.text.clone());
                    }
                } else if let Some(ref text) = reasoning.visible_text {
                    has_text = true;
                    if single_text.is_empty() {
                        single_text.push_str(text);
                    } else {
                        has_parts = true;
                        all_parts.push(single_text.clone());
                        single_text.clear();
                        all_parts.push(text.clone());
                    }
                }
            }
        }

        if has_parts {
            if !single_text.is_empty() {
                all_parts.push(single_text);
            }
            if all_parts.is_empty() {
                None
            } else {
                Some(ReasoningContent::Parts(all_parts))
            }
        } else if has_text && !single_text.is_empty() {
            Some(ReasoningContent::Text(single_text))
        } else {
            None
        }
    }

    /// Extract message text from all message items in output order.
    #[must_use]
    pub fn message_text(&self) -> Option<String> {
        let mut text = String::new();
        for item in &self.items {
            if let ProviderOutputPayload::Message { text: ref part } = item.payload {
                text.push_str(part);
            }
        }
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }
}

/// Errors returned when applying stream updates to a provider segment materializer.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MaterializerError {
    #[error("cannot update closed segment `{segment_id}` with outcome `{outcome:?}`")]
    ClosedSegmentImmutable {
        segment_id: String,
        outcome: ProviderSegmentOutcome,
    },
    #[error("segment id mismatch: expected `{expected}`, got `{actual}`")]
    SegmentIdMismatch { expected: String, actual: String },
    #[error("cannot close a provider segment that was never opened")]
    SegmentNeverOpened,
    #[error(
        "non-monotonic update sequence for item `{item_id}`: expected > {expected}, got {actual}"
    )]
    NonMonotonicSequence {
        item_id: String,
        expected: u64,
        actual: u64,
    },
    #[error(
        "position/id conflict: position {position} has item `{existing_id}`, but update is for `{item_id}`"
    )]
    PositionIdConflict {
        position: usize,
        existing_id: String,
        item_id: String,
    },
    #[error(
        "item `{item_id}` has position {existing_position}, but update specified {specified_position}"
    )]
    ItemPositionMismatch {
        item_id: String,
        existing_position: usize,
        specified_position: usize,
    },
    #[error("payload type mismatch for item `{item_id}`")]
    PayloadTypeMismatch { item_id: String },
}

/// Canonical materializer that constructs an ordered `ProviderSegment` from `ProviderItemUpdate`s.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSegmentMaterializer {
    pub segment: ProviderSegment,
    #[serde(default)]
    pub last_update_seq: HashMap<ProviderItemId, u64>,
}

impl ProviderSegmentMaterializer {
    #[must_use]
    pub fn new(segment_id: ProviderSegmentId) -> Self {
        Self {
            segment: ProviderSegment::new(segment_id),
            last_update_seq: HashMap::new(),
        }
    }

    /// Create a materializer whose segment identity is assigned by the provider
    /// on its first announced update.
    #[must_use]
    pub fn pending() -> Self {
        Self {
            segment: ProviderSegment::pending(),
            last_update_seq: HashMap::new(),
        }
    }

    /// Apply an append-only item update to this segment.
    pub fn apply_update(&mut self, update: &ProviderItemUpdate) -> Result<(), MaterializerError> {
        if self.segment.outcome != ProviderSegmentOutcome::Active {
            return Err(MaterializerError::ClosedSegmentImmutable {
                segment_id: self
                    .segment
                    .segment_id
                    .as_ref()
                    .map(|id| id.0.clone())
                    .unwrap_or_default(),
                outcome: self.segment.outcome,
            });
        }
        match self.segment.segment_id {
            Some(ref segment_id) if *segment_id != update.segment_id => {
                return Err(MaterializerError::SegmentIdMismatch {
                    expected: segment_id.0.clone(),
                    actual: update.segment_id.0.clone(),
                });
            },
            Some(_) => {},
            None => self.segment.segment_id = Some(update.segment_id.clone()),
        }
        if let Some(&last_seq) = self.last_update_seq.get(&update.item_id)
            && update.update_seq <= last_seq
        {
            return Err(MaterializerError::NonMonotonicSequence {
                item_id: update.item_id.0.clone(),
                expected: last_seq,
                actual: update.update_seq,
            });
        }
        self.last_update_seq
            .insert(update.item_id.clone(), update.update_seq);

        if let Some(existing) = self
            .segment
            .items
            .iter_mut()
            .find(|item| item.id == update.item_id)
        {
            if existing.position != update.position {
                return Err(MaterializerError::ItemPositionMismatch {
                    item_id: update.item_id.0.clone(),
                    existing_position: existing.position.0,
                    specified_position: update.position.0,
                });
            }
            Self::apply_payload_to_item(existing, &update.payload)?;
        } else {
            if let Some(existing) = self
                .segment
                .items
                .iter()
                .find(|item| item.position == update.position)
            {
                return Err(MaterializerError::PositionIdConflict {
                    position: update.position.0,
                    existing_id: existing.id.0.clone(),
                    item_id: update.item_id.0.clone(),
                });
            }
            let item = Self::create_item_from_update(update)?;
            self.segment.items.push(item);
            self.segment.items.sort_by_key(|item| item.position);
        }

        Ok(())
    }

    /// Mark the segment completed, failed, etc.
    pub fn close(&mut self, outcome: ProviderSegmentOutcome) -> Result<(), MaterializerError> {
        let Some(ref segment_id) = self.segment.segment_id else {
            return Err(MaterializerError::SegmentNeverOpened);
        };
        if self.segment.outcome != ProviderSegmentOutcome::Active {
            return Err(MaterializerError::ClosedSegmentImmutable {
                segment_id: segment_id.0.clone(),
                outcome: self.segment.outcome,
            });
        }
        self.segment.outcome = outcome;
        Ok(())
    }

    fn create_item_from_update(
        update: &ProviderItemUpdate,
    ) -> Result<ProviderOutputItem, MaterializerError> {
        let payload = match &update.payload {
            ProviderItemUpdatePayload::ReasoningDelta { part_index, delta } => {
                ProviderOutputPayload::Reasoning(ReasoningItem {
                    id: update.item_id.clone(),
                    output_index: update.position,
                    summary_parts: vec![ReasoningPart {
                        part_index: *part_index,
                        text: delta.clone(),
                    }],
                    visible_text: None,
                    encrypted_content: None,
                })
            },
            ProviderItemUpdatePayload::ReasoningPartDone { part_index, text } => {
                ProviderOutputPayload::Reasoning(ReasoningItem {
                    id: update.item_id.clone(),
                    output_index: update.position,
                    summary_parts: vec![ReasoningPart {
                        part_index: *part_index,
                        text: text.clone(),
                    }],
                    visible_text: None,
                    encrypted_content: None,
                })
            },
            ProviderItemUpdatePayload::ReasoningItemDone { encrypted_content } => {
                ProviderOutputPayload::Reasoning(ReasoningItem {
                    id: update.item_id.clone(),
                    output_index: update.position,
                    summary_parts: Vec::new(),
                    visible_text: None,
                    encrypted_content: encrypted_content.clone(),
                })
            },
            ProviderItemUpdatePayload::ReasoningText { text } => {
                ProviderOutputPayload::Reasoning(ReasoningItem {
                    id: update.item_id.clone(),
                    output_index: update.position,
                    summary_parts: Vec::new(),
                    visible_text: Some(text.clone()),
                    encrypted_content: None,
                })
            },
            ProviderItemUpdatePayload::ReasoningTextDelta { delta } => {
                ProviderOutputPayload::Reasoning(ReasoningItem {
                    id: update.item_id.clone(),
                    output_index: update.position,
                    summary_parts: Vec::new(),
                    visible_text: Some(delta.clone()),
                    encrypted_content: None,
                })
            },
            ProviderItemUpdatePayload::MessageDelta { delta } => ProviderOutputPayload::Message {
                text: delta.clone(),
            },
            ProviderItemUpdatePayload::MessageDone { text } => {
                ProviderOutputPayload::Message { text: text.clone() }
            },
            ProviderItemUpdatePayload::FunctionCallStart { name } => {
                ProviderOutputPayload::FunctionCall {
                    call_id: update.item_id.0.clone(),
                    name: name.clone(),
                    arguments: String::new(),
                }
            },
            ProviderItemUpdatePayload::FunctionCallDelta { delta } => {
                ProviderOutputPayload::FunctionCall {
                    call_id: update.item_id.0.clone(),
                    name: String::new(),
                    arguments: delta.clone(),
                }
            },
            ProviderItemUpdatePayload::FunctionCallDone { arguments } => {
                ProviderOutputPayload::FunctionCall {
                    call_id: update.item_id.0.clone(),
                    name: String::new(),
                    arguments: arguments.clone(),
                }
            },
        };

        Ok(ProviderOutputItem {
            id: update.item_id.clone(),
            position: update.position,
            payload,
        })
    }

    fn apply_payload_to_item(
        item: &mut ProviderOutputItem,
        payload: &ProviderItemUpdatePayload,
    ) -> Result<(), MaterializerError> {
        match (&mut item.payload, payload) {
            (
                ProviderOutputPayload::Reasoning(r),
                ProviderItemUpdatePayload::ReasoningDelta { part_index, delta },
            ) => {
                if let Some(part) = r
                    .summary_parts
                    .iter_mut()
                    .find(|p| p.part_index == *part_index)
                {
                    part.text.push_str(delta);
                } else {
                    r.summary_parts.push(ReasoningPart {
                        part_index: *part_index,
                        text: delta.clone(),
                    });
                    r.summary_parts.sort_by_key(|p| p.part_index);
                }
            },
            (
                ProviderOutputPayload::Reasoning(r),
                ProviderItemUpdatePayload::ReasoningPartDone { part_index, text },
            ) => {
                if let Some(part) = r
                    .summary_parts
                    .iter_mut()
                    .find(|p| p.part_index == *part_index)
                {
                    part.text = text.clone();
                } else {
                    r.summary_parts.push(ReasoningPart {
                        part_index: *part_index,
                        text: text.clone(),
                    });
                    r.summary_parts.sort_by_key(|p| p.part_index);
                }
            },
            (
                ProviderOutputPayload::Reasoning(r),
                ProviderItemUpdatePayload::ReasoningItemDone { encrypted_content },
            ) => {
                if encrypted_content.is_some() {
                    r.encrypted_content = encrypted_content.clone();
                }
            },
            (
                ProviderOutputPayload::Reasoning(r),
                ProviderItemUpdatePayload::ReasoningText { text },
            ) => {
                r.visible_text = Some(text.clone());
            },
            (
                ProviderOutputPayload::Reasoning(r),
                ProviderItemUpdatePayload::ReasoningTextDelta { delta },
            ) => {
                if let Some(ref mut text) = r.visible_text {
                    text.push_str(delta);
                } else {
                    r.visible_text = Some(delta.clone());
                }
            },
            (
                ProviderOutputPayload::Message { text },
                ProviderItemUpdatePayload::MessageDelta { delta },
            ) => {
                text.push_str(delta);
            },
            (
                ProviderOutputPayload::Message { text },
                ProviderItemUpdatePayload::MessageDone { text: final_text },
            ) => {
                *text = final_text.clone();
            },
            (
                ProviderOutputPayload::FunctionCall { name, .. },
                ProviderItemUpdatePayload::FunctionCallStart { name: new_name },
            ) => {
                *name = new_name.clone();
            },
            (
                ProviderOutputPayload::FunctionCall { arguments, .. },
                ProviderItemUpdatePayload::FunctionCallDelta { delta },
            ) => {
                arguments.push_str(delta);
            },
            (
                ProviderOutputPayload::FunctionCall { arguments, .. },
                ProviderItemUpdatePayload::FunctionCallDone {
                    arguments: final_args,
                },
            ) => {
                *arguments = final_args.clone();
            },
            _ => {
                return Err(MaterializerError::PayloadTypeMismatch {
                    item_id: item.id.0.clone(),
                });
            },
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn materializer_tracks_identity_and_positions_without_loss() {
        let mut mat = ProviderSegmentMaterializer::new(ProviderSegmentId::new("resp_1"));

        // Add reasoning item at position 0
        mat.apply_update(&ProviderItemUpdate {
            segment_id: ProviderSegmentId::new("resp_1"),
            item_id: ProviderItemId::new("rs_1"),
            position: ProviderItemPosition::new(0),
            update_seq: 1,
            payload: ProviderItemUpdatePayload::ReasoningDelta {
                part_index: 0,
                delta: "part 0 initial".to_string(),
            },
        })
        .unwrap();

        // Add function call at position 1
        mat.apply_update(&ProviderItemUpdate {
            segment_id: ProviderSegmentId::new("resp_1"),
            item_id: ProviderItemId::new("call_abc"),
            position: ProviderItemPosition::new(1),
            update_seq: 1,
            payload: ProviderItemUpdatePayload::FunctionCallStart {
                name: "search".to_string(),
            },
        })
        .unwrap();

        // Add delta to reasoning item
        mat.apply_update(&ProviderItemUpdate {
            segment_id: ProviderSegmentId::new("resp_1"),
            item_id: ProviderItemId::new("rs_1"),
            position: ProviderItemPosition::new(0),
            update_seq: 2,
            payload: ProviderItemUpdatePayload::ReasoningPartDone {
                part_index: 0,
                text: "part 0 final".to_string(),
            },
        })
        .unwrap();

        // Add reasoning item encrypted content
        mat.apply_update(&ProviderItemUpdate {
            segment_id: ProviderSegmentId::new("resp_1"),
            item_id: ProviderItemId::new("rs_1"),
            position: ProviderItemPosition::new(0),
            update_seq: 3,
            payload: ProviderItemUpdatePayload::ReasoningItemDone {
                encrypted_content: Some("enc_123".to_string()),
            },
        })
        .unwrap();

        // Function call arguments delta
        mat.apply_update(&ProviderItemUpdate {
            segment_id: ProviderSegmentId::new("resp_1"),
            item_id: ProviderItemId::new("call_abc"),
            position: ProviderItemPosition::new(1),
            update_seq: 2,
            payload: ProviderItemUpdatePayload::FunctionCallDelta {
                delta: "{\"q\":".to_string(),
            },
        })
        .unwrap();

        mat.apply_update(&ProviderItemUpdate {
            segment_id: ProviderSegmentId::new("resp_1"),
            item_id: ProviderItemId::new("call_abc"),
            position: ProviderItemPosition::new(1),
            update_seq: 3,
            payload: ProviderItemUpdatePayload::FunctionCallDone {
                arguments: "{\"q\":\"rust\"}".to_string(),
            },
        })
        .unwrap();

        mat.close(ProviderSegmentOutcome::Completed).unwrap();

        assert_eq!(mat.segment.items.len(), 2);
        assert_eq!(mat.segment.items[0].id.as_str(), "rs_1");
        assert_eq!(mat.segment.items[0].position.as_usize(), 0);
        assert_eq!(mat.segment.items[1].id.as_str(), "call_abc");
        assert_eq!(mat.segment.items[1].position.as_usize(), 1);

        match &mat.segment.items[0].payload {
            ProviderOutputPayload::Reasoning(r) => {
                assert_eq!(r.summary_parts[0].text, "part 0 final");
                assert_eq!(r.encrypted_content.as_deref(), Some("enc_123"));
            },
            _ => panic!("expected reasoning payload"),
        }

        match &mat.segment.items[1].payload {
            ProviderOutputPayload::FunctionCall {
                call_id,
                name,
                arguments,
            } => {
                assert_eq!(call_id, "call_abc");
                assert_eq!(name, "search");
                assert_eq!(arguments, "{\"q\":\"rust\"}");
            },
            _ => panic!("expected function_call payload"),
        }
    }

    #[test]
    fn materializer_rejects_non_monotonic_sequence() {
        let mut mat = ProviderSegmentMaterializer::new(ProviderSegmentId::new("resp_1"));
        mat.apply_update(&ProviderItemUpdate {
            segment_id: ProviderSegmentId::new("resp_1"),
            item_id: ProviderItemId::new("msg_1"),
            position: ProviderItemPosition::new(0),
            update_seq: 5,
            payload: ProviderItemUpdatePayload::MessageDelta {
                delta: "hello".to_string(),
            },
        })
        .unwrap();

        let err = mat
            .apply_update(&ProviderItemUpdate {
                segment_id: ProviderSegmentId::new("resp_1"),
                item_id: ProviderItemId::new("msg_1"),
                position: ProviderItemPosition::new(0),
                update_seq: 5,
                payload: ProviderItemUpdatePayload::MessageDelta {
                    delta: " world".to_string(),
                },
            })
            .unwrap_err();

        assert!(matches!(
            err,
            MaterializerError::NonMonotonicSequence { .. }
        ));
    }

    #[test]
    fn materializer_rejects_position_id_conflict() {
        let mut mat = ProviderSegmentMaterializer::new(ProviderSegmentId::new("resp_1"));
        mat.apply_update(&ProviderItemUpdate {
            segment_id: ProviderSegmentId::new("resp_1"),
            item_id: ProviderItemId::new("msg_1"),
            position: ProviderItemPosition::new(0),
            update_seq: 1,
            payload: ProviderItemUpdatePayload::MessageDelta {
                delta: "hello".to_string(),
            },
        })
        .unwrap();

        let err = mat
            .apply_update(&ProviderItemUpdate {
                segment_id: ProviderSegmentId::new("resp_1"),
                item_id: ProviderItemId::new("msg_2"),
                position: ProviderItemPosition::new(0),
                update_seq: 1,
                payload: ProviderItemUpdatePayload::MessageDelta {
                    delta: "world".to_string(),
                },
            })
            .unwrap_err();

        assert!(matches!(err, MaterializerError::PositionIdConflict { .. }));
    }

    #[test]
    fn materializer_closed_segment_is_immutable() {
        let mut mat = ProviderSegmentMaterializer::pending();
        mat.apply_update(&ProviderItemUpdate {
            segment_id: ProviderSegmentId::new("resp_1"),
            item_id: ProviderItemId::new("msg_0"),
            position: ProviderItemPosition::new(0),
            update_seq: 1,
            payload: ProviderItemUpdatePayload::MessageDelta {
                delta: "hi".to_string(),
            },
        })
        .unwrap();
        mat.close(ProviderSegmentOutcome::Completed).unwrap();

        let err = mat
            .apply_update(&ProviderItemUpdate {
                segment_id: ProviderSegmentId::new("resp_1"),
                item_id: ProviderItemId::new("msg_1"),
                position: ProviderItemPosition::new(0),
                update_seq: 1,
                payload: ProviderItemUpdatePayload::MessageDelta {
                    delta: "hello".to_string(),
                },
            })
            .unwrap_err();

        assert!(matches!(
            err,
            MaterializerError::ClosedSegmentImmutable { .. }
        ));
    }

    #[test]
    fn pending_materializer_adopts_provider_segment_identity() {
        let mut mat = ProviderSegmentMaterializer::pending();
        assert!(mat.segment.segment_id.is_none());

        mat.apply_update(&ProviderItemUpdate {
            segment_id: ProviderSegmentId::new("resp_live"),
            item_id: ProviderItemId::new("msg_0"),
            position: ProviderItemPosition::new(0),
            update_seq: 1,
            payload: ProviderItemUpdatePayload::MessageDelta {
                delta: "hello".to_string(),
            },
        })
        .unwrap();

        assert_eq!(
            mat.segment.segment_id,
            Some(ProviderSegmentId::new("resp_live"))
        );

        let err = mat
            .apply_update(&ProviderItemUpdate {
                segment_id: ProviderSegmentId::new("resp_other"),
                item_id: ProviderItemId::new("msg_0"),
                position: ProviderItemPosition::new(0),
                update_seq: 2,
                payload: ProviderItemUpdatePayload::MessageDelta {
                    delta: " world".to_string(),
                },
            })
            .unwrap_err();
        assert!(matches!(err, MaterializerError::SegmentIdMismatch { .. }));
    }

    #[test]
    fn closing_an_unopened_segment_is_rejected() {
        let mut mat = ProviderSegmentMaterializer::pending();
        assert_eq!(
            mat.close(ProviderSegmentOutcome::Completed),
            Err(MaterializerError::SegmentNeverOpened)
        );
    }

    #[test]
    fn serialization_roundtrip_preserves_everything() {
        let update = ProviderItemUpdate {
            segment_id: ProviderSegmentId::new("seg_1"),
            item_id: ProviderItemId::new("rs_1"),
            position: ProviderItemPosition::new(0),
            update_seq: 1,
            payload: ProviderItemUpdatePayload::ReasoningDelta {
                part_index: 0,
                delta: "delta text".to_string(),
            },
        };

        let json = serde_json::to_string(&update).unwrap();
        let decoded: ProviderItemUpdate = serde_json::from_str(&json).unwrap();
        assert_eq!(update, decoded);
    }
}
