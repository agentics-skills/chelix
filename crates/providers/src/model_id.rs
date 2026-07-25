//! Model ID manipulation: namespacing and raw ID extraction.

/// Separator between provider namespace and model ID.
pub(crate) const MODEL_ID_NAMESPACE_SEP: &str = "::";

#[must_use]
pub fn namespaced_model_id(provider: &str, model_id: &str) -> String {
    if model_id.contains(MODEL_ID_NAMESPACE_SEP) {
        return model_id.to_string();
    }
    format!("{provider}{MODEL_ID_NAMESPACE_SEP}{model_id}")
}

#[must_use]
pub fn raw_model_id(model_id: &str) -> &str {
    model_id
        .rsplit_once(MODEL_ID_NAMESPACE_SEP)
        .map(|(_, raw)| raw)
        .unwrap_or(model_id)
}

pub(crate) fn configured_model_for_provider(model_id: &str) -> &str {
    raw_model_id(model_id)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn raw_model_id_strips_provider_namespace() {
        assert_eq!(
            raw_model_id("anthropic::claude-opus-4-5"),
            "claude-opus-4-5"
        );
        assert_eq!(raw_model_id("gpt-4o"), "gpt-4o");
    }
}
