//! Public-boundary redaction of provider replay secrets.

use serde_json::Value;

/// Remove provider replay state before a value crosses a UI or API boundary.
pub fn redact_backend_only_provider_state(value: &mut Value) {
    match value {
        Value::Object(object) => {
            object.remove("responsesReasoning");
            object.remove("encrypted_content");
            object.remove("encryptedContent");
            object
                .values_mut()
                .for_each(redact_backend_only_provider_state);
        },
        Value::Array(items) => items
            .iter_mut()
            .for_each(redact_backend_only_provider_state),
        _ => {},
    }
}

#[cfg(test)]
mod tests {
    use super::redact_backend_only_provider_state;

    #[test]
    fn redacts_nested_replay_secrets_and_preserves_public_reasoning() {
        let mut value = serde_json::json!({
            "history": [{
                "id": "segment:reply",
                "reasoning": ["Check the result"],
                "providerItems": [{"id": "reasoning-1", "encryptedContent": "secret"}],
                "llmApiResponse": [{"item": {"encrypted_content": "secret", "id": "reasoning-1"}}],
                "responsesReasoning": [{"encryptedContent": "secret"}]
            }]
        });
        redact_backend_only_provider_state(&mut value);
        let message = &value["history"][0];
        assert!(message.get("responsesReasoning").is_none());
        assert!(
            message["providerItems"][0]
                .get("encryptedContent")
                .is_none()
        );
        assert!(
            message["llmApiResponse"][0]["item"]
                .get("encrypted_content")
                .is_none()
        );
        assert_eq!(message["reasoning"][0], "Check the result");
        assert_eq!(message["providerItems"][0]["id"], "reasoning-1");
    }
}
