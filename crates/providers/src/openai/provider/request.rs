use std::collections::{HashMap, HashSet};

use tracing::warn;

use chelix_agents::model::ChatMessage;

use {
    super::OpenAiProvider,
    crate::openai::{CacheControlPolicy, SystemMessageRewriteStrategy},
};

impl OpenAiProvider {
    /// Inject provider-configured `cache_control` breakpoints on the system
    /// message and the last user message.
    pub(super) fn apply_openrouter_cache_control(&self, messages: &mut [serde_json::Value]) {
        if !matches!(
            self.capabilities.cache_control_policy,
            CacheControlPolicy::OpenRouterAnthropic
        ) || matches!(self.cache_retention, chelix_config::CacheRetention::None)
        {
            return;
        }

        let cache_control = serde_json::json!({ "type": "ephemeral" });

        // Add cache_control to the system message content.
        for msg in messages.iter_mut() {
            if msg.get("role").and_then(serde_json::Value::as_str) != Some("system") {
                continue;
            }
            match msg.get_mut("content") {
                Some(content) if content.is_string() => {
                    let text = content.as_str().unwrap_or_default().to_string();
                    msg["content"] = serde_json::json!([{
                        "type": "text",
                        "text": text,
                        "cache_control": cache_control
                    }]);
                },
                Some(content) if content.is_array() => {
                    if let Some(last) = content.as_array_mut().and_then(|a| a.last_mut()) {
                        last["cache_control"] = cache_control.clone();
                    }
                },
                _ => {},
            }
            break;
        }

        // Add cache_control to the last user message.
        if let Some(last_user) = messages
            .iter_mut()
            .rev()
            .find(|m| m.get("role").and_then(serde_json::Value::as_str) == Some("user"))
        {
            match last_user.get_mut("content") {
                Some(content) if content.is_string() => {
                    let text = content.as_str().unwrap_or_default().to_string();
                    last_user["content"] = serde_json::json!([{
                        "type": "text",
                        "text": text,
                        "cache_control": cache_control
                    }]);
                },
                Some(content) if content.is_array() => {
                    if let Some(last) = content.as_array_mut().and_then(|a| a.last_mut()) {
                        last["cache_control"] = cache_control;
                    }
                },
                _ => {},
            }
        }
    }

    /// Convert raw tool schemas into the provider-compatible Chat
    /// Completions format.
    pub(super) fn prepare_chat_tools(
        &self,
        tools: &[serde_json::Value],
    ) -> anyhow::Result<Vec<serde_json::Value>> {
        crate::openai_compat::to_openai_tools(tools)
    }

    fn system_message_rewrite_strategy(&self) -> SystemMessageRewriteStrategy {
        if self.capabilities.requires_single_leading_system_message {
            return SystemMessageRewriteStrategy::MergeLeadingSystem;
        }
        SystemMessageRewriteStrategy::None
    }

    /// Rewrite system messages for providers with stricter chat template rules.
    ///
    /// Qwen-based OpenAI-compatible backends often only accept a single system
    /// message at the very front. For those, join all system messages with
    /// blank lines and emit exactly one leading `role: "system"` message.
    ///
    /// Must be called on the request body **after** it is fully assembled.
    pub(super) fn apply_system_prompt_rewrite(&self, body: &mut serde_json::Value) {
        let rewrite_strategy = self.system_message_rewrite_strategy();
        if matches!(rewrite_strategy, SystemMessageRewriteStrategy::None) {
            return;
        }
        let Some(messages) = body
            .get_mut("messages")
            .and_then(serde_json::Value::as_array_mut)
        else {
            return;
        };
        let mut system_parts = Vec::new();
        messages.retain(|msg| {
            if msg.get("role").and_then(serde_json::Value::as_str) == Some("system") {
                if let Some(content) = msg.get("content").and_then(serde_json::Value::as_str)
                    && !content.is_empty()
                {
                    system_parts.push(content.to_string());
                } else if msg.get("content").is_some() {
                    warn!(
                        ?rewrite_strategy,
                        "system message has non-string content; it will be dropped"
                    );
                }
                return false;
            }
            true
        });
        if system_parts.is_empty() {
            return;
        }
        let system_text = system_parts.join("\n\n");

        // MergeLeadingSystem: emit exactly one leading `role: "system"` message.
        messages.insert(
            0,
            serde_json::json!({
                "role": "system",
                "content": system_text,
            }),
        );
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

    use crate::openai::OpenAiProviderCapabilities;

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

    fn body_messages(body: &serde_json::Value) -> &[serde_json::Value] {
        let Some(messages) = body.get("messages").and_then(serde_json::Value::as_array) else {
            panic!("messages should be an array");
        };
        messages
    }

    #[test]
    fn explicit_system_message_policy_merges_multiple_messages() {
        let provider = provider("arbitrary-model", "custom", "https://example.invalid/v1")
            .with_capabilities(OpenAiProviderCapabilities {
                requires_single_leading_system_message: true,
                ..OpenAiProviderCapabilities::DEFAULT
            });
        let mut body = serde_json::json!({
            "messages": [
                {"role": "system", "content": "You are a helpful assistant."},
                {"role": "user", "content": "hello"},
                {"role": "assistant", "content": "hi"},
                {"role": "system", "content": "The current user datetime is 2026-04-15 18:22:00 UTC."},
                {"role": "user", "content": "what time is it?"}
            ]
        });

        provider.apply_system_prompt_rewrite(&mut body);

        let messages = body_messages(&body);
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(
            messages[0]["content"],
            "You are a helpful assistant.\n\nThe current user datetime is 2026-04-15 18:22:00 UTC."
        );
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[2]["role"], "assistant");
        assert_eq!(messages[3]["role"], "user");
    }

    #[test]
    fn system_message_rewrite_default_openai_request_is_unchanged() {
        let provider = provider("gpt-4o-mini", "openai", "https://api.openai.com/v1");
        let mut body = serde_json::json!({
            "messages": [
                {"role": "system", "content": "sys1"},
                {"role": "user", "content": "hello"},
                {"role": "system", "content": "sys2"}
            ]
        });

        provider.apply_system_prompt_rewrite(&mut body);

        let messages = body_messages(&body);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[2]["role"], "system");
    }

    #[test]
    fn model_name_does_not_enable_system_message_rewrite() {
        let provider = provider("qwen3-coder-plus", "openai", "https://api.openai.com/v1");
        let mut body = serde_json::json!({
            "messages": [
                {"role": "system", "content": "sys1"},
                {"role": "user", "content": "hello"},
                {"role": "system", "content": "sys2"}
            ]
        });

        provider.apply_system_prompt_rewrite(&mut body);

        let messages = body_messages(&body);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "sys1");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[2]["role"], "system");
        assert_eq!(messages[2]["content"], "sys2");
    }

    #[test]
    fn provider_policy_applies_without_model_name_matching() {
        let provider = provider(
            "arbitrary-model",
            "alibaba-coding",
            "https://coding-intl.dashscope.aliyuncs.com/v1",
        )
        .with_capabilities(OpenAiProviderCapabilities {
            requires_single_leading_system_message: true,
            ..OpenAiProviderCapabilities::DEFAULT
        });
        let mut body = serde_json::json!({
            "messages": [
                {"role": "system", "content": "sys1"},
                {"role": "user", "content": "hello"},
                {"role": "system", "content": "sys2"}
            ]
        });

        provider.apply_system_prompt_rewrite(&mut body);

        let messages = body_messages(&body);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "sys1\n\nsys2");
        assert_eq!(messages[1]["role"], "user");
    }

    #[test]
    fn openrouter_cache_control_is_capability_driven() {
        let p = provider(
            "anthropic/claude-sonnet-4-20250514",
            "aliased-openrouter",
            "https://example.invalid/v1",
        )
        .with_capabilities(OpenAiProviderCapabilities {
            cache_control_policy: CacheControlPolicy::OpenRouterAnthropic,
            ..OpenAiProviderCapabilities::DEFAULT
        });
        let mut messages = vec![
            serde_json::json!({"role": "system", "content": "sys"}),
            serde_json::json!({"role": "user", "content": "hello"}),
        ];

        p.apply_openrouter_cache_control(&mut messages);

        assert_eq!(
            messages[0]["content"][0]["cache_control"],
            serde_json::json!({"type": "ephemeral"})
        );
        assert_eq!(
            messages[1]["content"][0]["cache_control"],
            serde_json::json!({"type": "ephemeral"})
        );
    }

    #[test]
    fn custom_provider_with_openrouter_url_does_not_get_cache_control() {
        let p = provider(
            "anthropic/claude-sonnet-4-20250514",
            "custom-openrouter",
            "https://openrouter.ai/api/v1",
        );
        let mut messages = vec![
            serde_json::json!({"role": "system", "content": "sys"}),
            serde_json::json!({"role": "user", "content": "hello"}),
        ];

        p.apply_openrouter_cache_control(&mut messages);

        assert_eq!(messages[0]["content"], "sys");
        assert_eq!(messages[1]["content"], "hello");
    }

    /// OpenAI provider must preserve the (sanitized) `name` field.
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
