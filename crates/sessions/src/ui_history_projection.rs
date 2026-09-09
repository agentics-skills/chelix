//! Incremental semantic projection of copied provider and journal events.

use chelix_common::{
    ProviderOutputPayload, ProviderSegmentId, ProviderSegmentMaterializer, ProviderSegmentOutcome,
    tool_lifecycle::ToolLifecycleUpdate,
};

use crate::{
    Error, PersistedMessage, Result,
    message::{PersistedFunction, PersistedToolCall},
    ui_history_types::{
        UiContent, UiEntry, UiMessageId, UiPresentation, UiRecord, UiRunMetadata, UiSnapshot,
    },
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum UiIngress {
    Live,
    Journal,
}

pub(crate) fn identity(record: &UiRecord) -> Result<Option<UiMessageId>> {
    Ok(match &record.message {
        PersistedMessage::ProviderUpdate { update, .. } => {
            Some(UiMessageId::segment(&update.segment_id))
        },
        PersistedMessage::ProviderSegmentClose { segment_id, .. }
        | PersistedMessage::Assistant {
            segment_id: Some(segment_id),
            ..
        } => Some(UiMessageId::segment(segment_id)),
        PersistedMessage::ToolLifecycle { lifecycle } => Some(UiMessageId::tool(
            lifecycle
                .run_id
                .as_deref()
                .ok_or_else(|| Error::message("UI tool lifecycle requires a run ID"))?,
            &lifecycle.tool_call_id,
        )),
        PersistedMessage::User { .. } => record
            .client_message_id
            .as_ref()
            .map(|id| -> Result<UiMessageId> {
                crate::ui_history_types::validate_client_message_id(id)?;
                Ok(UiMessageId(format!("user:{id}")))
            })
            .transpose()?,
        _ => None,
    })
}

fn empty_assistant(segment_id: &ProviderSegmentId, run: Option<&UiRunMetadata>) -> UiRecord {
    PersistedMessage::Assistant {
        content: String::new(),
        created_at: None,
        model: run.map(|run| run.model.clone()),
        provider: run.map(|run| run.provider.clone()),
        reasoning_effort: run.and_then(|run| run.reasoning_effort.clone()),
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        duration_ms: None,
        request_input_tokens: None,
        request_output_tokens: None,
        request_cache_read_tokens: None,
        request_cache_write_tokens: None,
        tool_calls: None,
        reasoning: None,
        provider_items: None,
        segment_id: Some(segment_id.clone()),
        llm_api_response: None,
        audio: None,
        seq: None,
        run_id: run.map(|run| run.run_id.clone()),
    }
    .into()
}

pub(crate) fn initial_entry(id: UiMessageId, position: u64, record: UiRecord) -> UiEntry {
    UiEntry {
        snapshot: UiSnapshot {
            id,
            position,
            revision: 0,
            canonical_committed: false,
            content: UiContent::Record(Box::new(record)),
            presentation: UiPresentation::default(),
            accumulated_arguments: None,
            assistant_id: None,
            outcome: None,
        },
        canonical: None,
        content_version: 0,
        materializer: None,
    }
}

/// Returns false for the journal receipt of an event already copied live.
pub(crate) fn project(
    entry: &mut UiEntry,
    mut record: UiRecord,
    ingress: UiIngress,
    run: Option<&UiRunMetadata>,
) -> Result<bool> {
    if let PersistedMessage::Assistant {
        llm_api_response, ..
    } = &mut record.message
    {
        *llm_api_response = None;
    }
    match &record.message {
        PersistedMessage::ProviderUpdate {
            update,
            created_at,
            seq,
            run_id,
        } => {
            let materializer = entry
                .materializer
                .get_or_insert_with(|| ProviderSegmentMaterializer::new(update.segment_id.clone()));
            if ingress == UiIngress::Journal
                && materializer
                    .last_update_seq
                    .get(&update.item_id)
                    .is_some_and(|sequence| *sequence >= update.update_seq)
            {
                return Ok(false);
            }
            materializer
                .apply_update(update)
                .map_err(|error| Error::message(error.to_string()))?;
            let mut assistant = match &entry.snapshot.content {
                UiContent::Record(existing)
                    if matches!(existing.message, PersistedMessage::Assistant { .. }) =>
                {
                    (**existing).clone()
                },
                _ => empty_assistant(&update.segment_id, run),
            };
            if let PersistedMessage::Assistant {
                content,
                created_at: timestamp,
                seq: assistant_seq,
                run_id: assistant_run,
                provider_items,
                reasoning,
                tool_calls,
                ..
            } = &mut assistant.message
            {
                *content = materializer.segment.message_text().unwrap_or_default();
                *timestamp = timestamp.or(*created_at);
                *assistant_seq = *seq;
                *assistant_run = run_id.clone().or_else(|| run.map(|run| run.run_id.clone()));
                *reasoning = materializer.segment.reasoning_content();
                *provider_items = Some(materializer.segment.items.clone());
                let calls = materializer
                    .segment
                    .items
                    .iter()
                    .filter_map(|item| {
                        let ProviderOutputPayload::FunctionCall {
                            call_id,
                            name,
                            arguments,
                        } = &item.payload
                        else {
                            return None;
                        };
                        Some(PersistedToolCall {
                            id: call_id.clone(),
                            call_type: "function".to_string(),
                            function: PersistedFunction {
                                name: name.clone(),
                                arguments: arguments.clone(),
                            },
                        })
                    })
                    .collect::<Vec<_>>();
                *tool_calls = (!calls.is_empty()).then_some(calls);
            }
            entry.snapshot.content = UiContent::Record(Box::new(assistant));
            entry.snapshot.outcome = Some(materializer.segment.outcome);
        },
        PersistedMessage::ProviderSegmentClose {
            segment_id,
            outcome,
            ..
        } => {
            let materializer = entry
                .materializer
                .get_or_insert_with(|| ProviderSegmentMaterializer::new(segment_id.clone()));
            if ingress == UiIngress::Journal && materializer.segment.outcome == *outcome {
                return Ok(false);
            }
            materializer
                .close(*outcome)
                .map_err(|error| Error::message(error.to_string()))?;
            if !matches!(&entry.snapshot.content, UiContent::Record(record) if matches!(record.message, PersistedMessage::Assistant { .. }))
            {
                entry.snapshot.content =
                    UiContent::Record(Box::new(empty_assistant(segment_id, run)));
            }
            entry.snapshot.outcome = Some(*outcome);
        },
        PersistedMessage::ToolLifecycle { lifecycle } => {
            if let UiContent::Record(existing) = &entry.snapshot.content
                && let PersistedMessage::ToolLifecycle {
                    lifecycle: previous,
                } = &existing.message
                && entry.snapshot.revision > 0
                && lifecycle.sequence <= previous.sequence
            {
                if ingress == UiIngress::Journal {
                    return Ok(false);
                }
                return Err(Error::message("non-monotonic UI tool lifecycle sequence"));
            }
            if let ToolLifecycleUpdate::InputStreaming { arguments_delta } = &lifecycle.update {
                entry
                    .snapshot
                    .accumulated_arguments
                    .get_or_insert_with(String::new)
                    .push_str(arguments_delta);
            }
            entry.snapshot.content = UiContent::Record(Box::new(record));
        },
        PersistedMessage::Assistant {
            segment_id,
            provider_items,
            ..
        } => {
            if let Some(materializer) = &entry.materializer
                && (materializer.segment.segment_id.as_ref() != segment_id.as_ref()
                    || provider_items
                        .as_deref()
                        .is_some_and(|items| items != materializer.segment.items))
            {
                return Err(Error::message(
                    "assistant snapshot differs from copied canonical provider items",
                ));
            }
            entry.snapshot.content = UiContent::Record(Box::new(record));
        },
        PersistedMessage::Tool { .. } => {
            return Err(Error::message(
                "tool results require a UI lifecycle identity",
            ));
        },
        PersistedMessage::System { .. }
        | PersistedMessage::Notice { .. }
        | PersistedMessage::Checkpoint { .. }
        | PersistedMessage::User { .. } => {
            entry.snapshot.content = UiContent::Record(Box::new(record));
        },
    }
    Ok(true)
}

pub(crate) fn active(entry: &UiEntry) -> bool {
    if entry.snapshot.outcome == Some(ProviderSegmentOutcome::Active) {
        return true;
    }
    matches!(&entry.snapshot.content, UiContent::Record(record)
        if matches!(&record.message, PersistedMessage::ToolLifecycle { lifecycle } if !lifecycle.stage().is_terminal()))
}

pub(crate) fn search_text(snapshot: &UiSnapshot) -> Result<String> {
    let value = snapshot.public_value()?;
    let mut text = Vec::new();
    collect_text(&value, &mut text);
    Ok(text.join("\n"))
}

pub(crate) fn search_snippet(text: &str, query: &str) -> Option<String> {
    let lowercase = text.to_lowercase();
    let match_start = lowercase.find(query)?;
    let match_end = match_start + query.len();
    let mut offset = 0;
    let mut source_start = None;
    let mut source_end = text.len();
    for (index, character) in text.char_indices() {
        let next = offset + character.to_lowercase().map(char::len_utf8).sum::<usize>();
        if source_start.is_none() && next > match_start {
            source_start = Some(index);
        }
        if next >= match_end {
            source_end = index + character.len_utf8();
            break;
        }
        offset = next;
    }
    let start = text.floor_char_boundary(source_start?.saturating_sub(40));
    let end = text.floor_char_boundary(source_end.saturating_add(60).min(text.len()));
    Some(text[start..end].to_string())
}

fn collect_text(value: &serde_json::Value, output: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => output.push(text.clone()),
        serde_json::Value::Array(values) => {
            values.iter().for_each(|value| collect_text(value, output))
        },
        serde_json::Value::Object(fields) => {
            for key in [
                "content",
                "text",
                "summary",
                "reasoning",
                "arguments",
                "accumulatedArguments",
                "presentation",
                "toolName",
                "result",
                "error",
                "raw",
                "detail",
                "document",
            ] {
                if let Some(value) = fields.get(key) {
                    collect_text(value, output);
                }
            }
        },
        _ => {},
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod search_tests {
    use super::search_snippet;

    #[test]
    fn snippets_are_bounded_and_preserve_source_case_and_unicode_boundaries() {
        for prefix in ["a".repeat(200), "İK".repeat(200), "Ж".repeat(200)] {
            let text = format!("{prefix}NeEdLe{}", "z".repeat(200));
            let snippet = search_snippet(&text, "needle").unwrap();
            assert!(snippet.contains("NeEdLe"));
            assert!(snippet.len() <= 109);
            assert!(!snippet.contains(&prefix));
        }
        assert_eq!(search_snippet("İK", "k").as_deref(), Some("İK"));
        assert_eq!(search_snippet("absent", "needle"), None);
    }
}
