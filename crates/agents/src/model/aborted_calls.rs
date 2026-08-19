//! Synthetic results for tool calls that never produced one.
//!
//! A provider request rejects an assistant message whose tool call has no
//! matching result, so a call interrupted mid-stream would make every later
//! request fail. The interrupted call itself must survive — a retry closes a
//! segment but never deletes what the provider already produced — so the gap is
//! filled with an explicit aborted result placed directly after its call.

use super::chat::ChatMessage;

/// Result text recorded for a tool call the run never completed.
const ABORTED_TOOL_RESULT: &str = "aborted";

/// Insert an aborted result after every tool call that has none.
///
/// The insertion is reported through `tracing::warn!`: losing a tool call to an
/// interrupted stream is not part of a normal turn, and the synthetic result
/// must stay visible in the logs rather than silently repair the conversation.
pub(crate) fn ensure_tool_call_results_present(messages: &mut Vec<ChatMessage>) {
    let answered: std::collections::HashSet<&str> = messages
        .iter()
        .filter_map(|message| match message {
            ChatMessage::Tool { tool_call_id, .. } => Some(tool_call_id.as_str()),
            _ => None,
        })
        .collect();

    let mut missing: Vec<(usize, String)> = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        let ChatMessage::Assistant {
            tool_calls,
            provider_items,
            ..
        } = message
        else {
            continue;
        };
        // A call reaches the provider through `tool_calls` on Chat Completions
        // and through `provider_items` on Responses, so both carry calls that
        // need a result.
        let call_ids = tool_calls
            .iter()
            .map(|tool_call| tool_call.id.as_str())
            .chain(
                provider_items
                    .iter()
                    .filter_map(|item| match &item.payload {
                        chelix_common::ProviderOutputPayload::FunctionCall { call_id, .. } => {
                            Some(call_id.as_str())
                        },
                        _ => None,
                    }),
            );
        for call_id in call_ids {
            if answered.contains(call_id) {
                continue;
            }
            if missing.iter().any(|(_, pending)| pending == call_id) {
                continue;
            }
            missing.push((index, call_id.to_owned()));
        }
    }

    // Insert from the back so the recorded indices stay valid.
    for (index, call_id) in missing.into_iter().rev() {
        tracing::warn!(
            tool_call_id = %call_id,
            "tool call has no result; recording it as aborted so the next request stays valid"
        );
        messages.insert(index + 1, ChatMessage::tool(call_id, ABORTED_TOOL_RESULT));
    }
}

#[cfg(test)]
mod tests {
    use {super::*, crate::model::ToolCall};

    fn assistant_with_call(call_id: &str) -> ChatMessage {
        ChatMessage::Assistant {
            content: None,
            tool_calls: vec![ToolCall {
                id: call_id.to_owned(),
                name: "search".to_owned(),
                arguments: serde_json::json!({}),
                argument_diagnostic: None,
            }],
            reasoning: None,
            provider_items: Vec::new(),
            segment_id: None,
        }
    }

    #[test]
    fn a_call_without_a_result_gets_an_aborted_result_right_after_it() {
        let mut messages = vec![
            ChatMessage::user("hi"),
            assistant_with_call("call_1"),
            ChatMessage::user("still there?"),
        ];

        ensure_tool_call_results_present(&mut messages);

        let ChatMessage::Tool {
            tool_call_id,
            content,
        } = &messages[2]
        else {
            panic!("expected an aborted result directly after its call");
        };
        assert_eq!(tool_call_id, "call_1");
        assert_eq!(content, "aborted");
        assert_eq!(messages.len(), 4);
    }

    #[test]
    fn a_call_that_already_has_a_result_is_left_alone() {
        let mut messages = vec![
            assistant_with_call("call_1"),
            ChatMessage::tool("call_1", "done"),
        ];

        ensure_tool_call_results_present(&mut messages);

        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn a_provider_item_call_without_a_result_is_covered_too() {
        let mut messages = vec![ChatMessage::Assistant {
            content: None,
            tool_calls: Vec::new(),
            reasoning: None,
            provider_items: vec![chelix_common::ProviderOutputItem {
                id: chelix_common::ProviderItemId("item_1".to_owned()),
                position: chelix_common::ProviderItemPosition(0),
                payload: chelix_common::ProviderOutputPayload::FunctionCall {
                    call_id: "call_1".to_owned(),
                    name: "search".to_owned(),
                    arguments: "{\"query\":".to_owned(),
                },
            }],
            segment_id: None,
        }];

        ensure_tool_call_results_present(&mut messages);

        let ChatMessage::Tool { tool_call_id, .. } = &messages[1] else {
            panic!("expected an aborted result for the interrupted call");
        };
        assert_eq!(tool_call_id, "call_1");
    }

    #[test]
    fn two_unanswered_calls_each_get_their_own_result() {
        let mut messages = vec![assistant_with_call("call_1"), assistant_with_call("call_2")];

        ensure_tool_call_results_present(&mut messages);

        let ids: Vec<&str> = messages
            .iter()
            .filter_map(|message| match message {
                ChatMessage::Tool { tool_call_id, .. } => Some(tool_call_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(ids, vec!["call_1", "call_2"]);
        assert_eq!(messages.len(), 4);
    }
}
