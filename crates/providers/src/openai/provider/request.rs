use std::collections::{HashMap, HashSet};

use chelix_agents::model::ChatMessage;

use super::OpenAiProvider;

impl OpenAiProvider {
    /// Completions format.
    pub(super) fn prepare_chat_tools(
        &self,
        tools: &[serde_json::Value],
    ) -> anyhow::Result<Vec<serde_json::Value>> {
        crate::openai_compat::to_openai_tools(tools)
    }

    pub(super) fn serialize_messages_for_request(
        &self,
        messages: &[ChatMessage],
    ) -> Vec<serde_json::Value> {
        let mut remapped_tool_call_ids = HashMap::new();
        let mut used_tool_call_ids = HashSet::new();
        let mut out = Vec::with_capacity(messages.len());

        for message in messages {
            let mut value = message.to_openai_value();

            if let Some(tool_calls) = value
                .get_mut("tool_calls")
                .and_then(serde_json::Value::as_array_mut)
            {
                for tool_call in tool_calls {
                    let Some(tool_call_id) =
                        tool_call.get("id").and_then(serde_json::Value::as_str)
                    else {
                        continue;
                    };
                    let mapped_id = assign_openai_tool_call_id(
                        tool_call_id,
                        &mut remapped_tool_call_ids,
                        &mut used_tool_call_ids,
                    );
                    tool_call["id"] = serde_json::Value::String(mapped_id);
                }
            } else if value.get("role").and_then(serde_json::Value::as_str) == Some("tool")
                && let Some(tool_call_id) = value
                    .get("tool_call_id")
                    .and_then(serde_json::Value::as_str)
            {
                let mapped_id = remapped_tool_call_ids
                    .get(tool_call_id)
                    .cloned()
                    .unwrap_or_else(|| {
                        assign_openai_tool_call_id(
                            tool_call_id,
                            &mut remapped_tool_call_ids,
                            &mut used_tool_call_ids,
                        )
                    });
                value["tool_call_id"] = serde_json::Value::String(mapped_id);
            }

            out.push(value);
        }

        out
    }
}

const OPENAI_MAX_TOOL_CALL_ID_LEN: usize = 40;

fn short_stable_hash(value: &str) -> String {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn base_openai_tool_call_id(raw: &str) -> String {
    let mut cleaned: String = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect();

    if cleaned.is_empty() {
        cleaned = "call".to_string();
    }

    if cleaned.len() <= OPENAI_MAX_TOOL_CALL_ID_LEN {
        return cleaned;
    }

    let hash = short_stable_hash(raw);
    let keep = OPENAI_MAX_TOOL_CALL_ID_LEN.saturating_sub(hash.len() + 1);
    cleaned.truncate(keep);
    if cleaned.is_empty() {
        return format!("call-{hash}");
    }
    format!("{cleaned}-{hash}")
}

fn disambiguate_tool_call_id(base: &str, nonce: usize) -> String {
    let suffix = format!("-{nonce}");
    let keep = OPENAI_MAX_TOOL_CALL_ID_LEN.saturating_sub(suffix.len());

    let mut value = base.to_string();
    if value.len() > keep {
        value.truncate(keep);
    }
    if value.is_empty() {
        value = "call".to_string();
        if value.len() > keep {
            value.truncate(keep);
        }
    }
    format!("{value}{suffix}")
}

fn assign_openai_tool_call_id(
    raw: &str,
    remapped_tool_call_ids: &mut HashMap<String, String>,
    used_tool_call_ids: &mut HashSet<String>,
) -> String {
    if let Some(existing) = remapped_tool_call_ids.get(raw) {
        return existing.clone();
    }

    let base = base_openai_tool_call_id(raw);
    let mut candidate = base.clone();
    let mut nonce = 1usize;
    while used_tool_call_ids.contains(&candidate) {
        candidate = disambiguate_tool_call_id(&base, nonce);
        nonce = nonce.saturating_add(1);
    }

    used_tool_call_ids.insert(candidate.clone());
    remapped_tool_call_ids.insert(raw.to_string(), candidate.clone());
    candidate
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use secrecy::Secret;

    use super::*;

    fn next_test_secret_id() -> u64 {
        static NEXT_TEST_SECRET_ID: AtomicU64 = AtomicU64::new(1);
        NEXT_TEST_SECRET_ID.fetch_add(1, Ordering::Relaxed)
    }

    fn generated_api_key() -> Secret<String> {
        Secret::new(format!("k{:016x}", next_test_secret_id()))
    }

    fn provider(model: &str, provider_name: &str, base_url: &str) -> OpenAiProvider {
        OpenAiProvider::new_with_name(
            generated_api_key(),
            model.to_string(),
            base_url.to_string(),
            provider_name.to_string(),
        )
    }

    #[test]
    fn openai_provider_preserves_user_name() {
        let p = provider("gpt-4o", "openai", "https://api.openai.com/v1");

        let messages = vec![ChatMessage::user_named("hello", "Alice")];
        let serialized = p.serialize_messages_for_request(&messages);
        assert_eq!(serialized[0]["name"], "Alice");
    }

    #[test]
    fn openai_provider_trims_base_url_and_api_key_edges() {
        let p = OpenAiProvider::new_with_name(
            Secret::new(" test-key\n".to_string()),
            "gpt-4o".to_string(),
            " https://api.openai.com/v1/ \n".to_string(),
            "openai".to_string(),
        );

        assert_eq!(
            p.chat_completions_url(),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(p.responses_sse_url(), "https://api.openai.com/v1/responses");
        assert_eq!(p.bearer_auth_header(), "Bearer test-key");
    }
}
