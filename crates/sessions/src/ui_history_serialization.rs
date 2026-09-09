//! Typed serialization of semantic assistant snapshots.

use {
    chelix_common::{ProviderOutputItem, ProviderSegmentId, ReasoningContent},
    serde::{Serialize, Serializer},
};

use crate::{PersistedMessage, message::PersistedToolCall, ui_history_types::UiContent};

#[cfg(test)]
#[path = "ui_history_serialization_tests.rs"]
mod tests;

#[derive(Serialize)]
#[serde(tag = "role", rename_all = "lowercase")]
enum AssistantMessage<'a> {
    Assistant {
        content: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        created_at: &'a Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        model: &'a Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        provider: &'a Option<String>,
        #[serde(rename = "reasoningEffort", skip_serializing_if = "Option::is_none")]
        reasoning_effort: &'a Option<String>,
        #[serde(rename = "inputTokens", skip_serializing_if = "Option::is_none")]
        input_tokens: &'a Option<u32>,
        #[serde(rename = "outputTokens", skip_serializing_if = "Option::is_none")]
        output_tokens: &'a Option<u32>,
        #[serde(rename = "cacheReadTokens", skip_serializing_if = "Option::is_none")]
        cache_read_tokens: &'a Option<u32>,
        #[serde(rename = "cacheWriteTokens", skip_serializing_if = "Option::is_none")]
        cache_write_tokens: &'a Option<u32>,
        #[serde(rename = "durationMs", skip_serializing_if = "Option::is_none")]
        duration_ms: &'a Option<u64>,
        #[serde(rename = "requestInputTokens", skip_serializing_if = "Option::is_none")]
        request_input_tokens: &'a Option<u32>,
        #[serde(
            rename = "requestOutputTokens",
            skip_serializing_if = "Option::is_none"
        )]
        request_output_tokens: &'a Option<u32>,
        #[serde(
            rename = "requestCacheReadTokens",
            skip_serializing_if = "Option::is_none"
        )]
        request_cache_read_tokens: &'a Option<u32>,
        #[serde(
            rename = "requestCacheWriteTokens",
            skip_serializing_if = "Option::is_none"
        )]
        request_cache_write_tokens: &'a Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_calls: &'a Option<Vec<PersistedToolCall>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning: &'a Option<ReasoningContent>,
        #[serde(rename = "providerItems", skip_serializing_if = "Option::is_none")]
        provider_items: &'a Option<Vec<ProviderOutputItem>>,
        #[serde(rename = "segmentId", skip_serializing_if = "Option::is_none")]
        segment_id: &'a Option<ProviderSegmentId>,
        #[serde(skip_serializing_if = "Option::is_none")]
        audio: &'a Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        seq: &'a Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        run_id: &'a Option<String>,
    },
}

#[derive(Serialize)]
struct AssistantRecord<'a> {
    #[serde(flatten)]
    message: AssistantMessage<'a>,
    #[serde(rename = "clientMessageId", skip_serializing_if = "Option::is_none")]
    client_message_id: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tts_provider: &'a Option<String>,
}

pub(crate) fn serialize_content<S>(content: &UiContent, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let UiContent::Record(record) = content else {
        return content.serialize(serializer);
    };
    let PersistedMessage::Assistant {
        content,
        created_at,
        model,
        provider,
        reasoning_effort,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        duration_ms,
        request_input_tokens,
        request_output_tokens,
        request_cache_read_tokens,
        request_cache_write_tokens,
        tool_calls,
        reasoning,
        provider_items,
        segment_id,
        llm_api_response: _,
        audio,
        seq,
        run_id,
    } = &record.message
    else {
        return content.serialize(serializer);
    };
    AssistantRecord {
        message: AssistantMessage::Assistant {
            content,
            created_at,
            model,
            provider,
            reasoning_effort,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            duration_ms,
            request_input_tokens,
            request_output_tokens,
            request_cache_read_tokens,
            request_cache_write_tokens,
            tool_calls,
            reasoning,
            provider_items,
            segment_id,
            audio,
            seq,
            run_id,
        },
        client_message_id: &record.client_message_id,
        tts_provider: &record.tts_provider,
    }
    .serialize(serializer)
}
