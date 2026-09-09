//! Typed parameter structs for complex session RPC methods.
//!
//! Only methods with non-trivial parameter shapes (multi-field with defaults,
//! null-vs-absent semantics, precedence logic) get dedicated structs here.
//! Simple key-only handlers use inline `.get(...)` directly.

use serde::Deserialize;

use {
    crate::services::ServiceError, chelix_common::ReasoningEffort,
    chelix_sessions::ui_history_types::UiHistoryTarget,
};

/// Params for `sessions.patch`.
///
/// All fields except `key` are optional — only provided fields are updated.
///
/// Fields with meaningful `null` values use `Option<Option<T>>`:
/// - outer `None` → field was absent from the request (no-op)
/// - `Some(None)` → field was explicitly `null`
/// - `Some(Some(v))` → field was set to value `v`
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PatchParams {
    pub key: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    pub model: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub reasoning_effort: Option<Option<ReasoningEffort>>,
    #[serde(default)]
    pub archived: Option<bool>,
    #[serde(default, deserialize_with = "double_option")]
    pub project_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub worktree_branch: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub mcp_disabled: Option<Option<bool>>,
    #[serde(default, deserialize_with = "double_option")]
    pub parent_session_key: Option<Option<String>>,
}

/// Deserialize a field as `Some(inner)` when present (even if null),
/// vs `None` when absent (via `#[serde(default)]`).
fn double_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::<T>::deserialize(deserializer)?))
}

/// Params for `session.voice_generate`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VoiceGenerateParams {
    pub key: String,
    pub target: UiHistoryTarget,
}

/// Params for `sessions.fork`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForkParams {
    pub key: String,
    pub label: Option<String>,
    pub target: Option<UiHistoryTarget>,
    pub fork_point: Option<u64>,
}

/// Params for `sessions.truncate_tail`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TruncateTailParams {
    pub key: String,
    pub target: UiHistoryTarget,
}

impl TruncateTailParams {
    /// Return a non-empty session key.
    pub fn key(&self) -> Result<&str, &'static str> {
        let key = self.key.trim();
        if key.is_empty() {
            return Err("missing 'key' parameter");
        }
        Ok(key)
    }
}

/// Parse a `serde_json::Value` into a typed param struct, mapping
/// deserialization errors to the service error format.
pub fn parse_params<T: serde::de::DeserializeOwned>(
    params: serde_json::Value,
) -> Result<T, ServiceError> {
    serde_json::from_value(params).map_err(ServiceError::message)
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use {super::*, serde_json::json};

    #[test]
    fn patch_params_accepts_canonical_payload() {
        let p: PatchParams = serde_json::from_value(json!({
            "key": "main",
            "label": "My Chat",
            "model": "openai::gpt-5.2",
            "reasoningEffort": "high",
            "archived": true,
            "projectId": null,
            "worktreeBranch": "feature/abc",
            "mcpDisabled": false,
            "parentSessionKey": null,
        }))
        .unwrap();
        assert_eq!(p.key, "main");
        assert_eq!(p.label.as_deref(), Some("My Chat"));
        assert_eq!(
            p.model.as_ref().and_then(Option::as_deref),
            Some("openai::gpt-5.2")
        );
        assert_eq!(
            p.reasoning_effort
                .as_ref()
                .and_then(Option::as_ref)
                .map(ReasoningEffort::as_str),
            Some("high"),
        );
        assert_eq!(p.archived, Some(true));
        assert!(matches!(p.project_id, Some(None)));
        assert_eq!(p.worktree_branch, Some(Some("feature/abc".to_string())));
        assert_eq!(p.mcp_disabled, Some(Some(false)));
        assert!(matches!(p.parent_session_key, Some(None)));
    }

    #[test]
    fn patch_params_rejects_additional_field() {
        let result: Result<PatchParams, _> = serde_json::from_value(json!({
            "key": "main",
            "additionalField": true,
        }));
        assert!(result.is_err());
    }

    #[test]
    fn history_actions_require_identity_and_generation() {
        let value = json!({
            "key": "main",
            "target": {"messageId": "segment:answer", "generation": "generation-1"},
        });
        let voice: VoiceGenerateParams = serde_json::from_value(value.clone()).unwrap();
        let truncate: TruncateTailParams = serde_json::from_value(value).unwrap();
        assert_eq!(voice.target.message_id.0, "segment:answer");
        assert_eq!(truncate.target.generation.0, "generation-1");
    }

    #[test]
    fn voice_generate_rejects_obsolete_targets() {
        for target in [
            json!({
                "key": "main",
                "runId": "run-abc",
            }),
            json!({
                "key": "main",
                "historyIndex": 7,
            }),
        ] {
            assert!(serde_json::from_value::<VoiceGenerateParams>(target).is_err());
        }
    }

    #[test]
    fn voice_generate_rejects_missing_target() {
        let result = serde_json::from_value::<VoiceGenerateParams>(json!({
            "key": "main",
        }));
        assert!(result.is_err());
    }

    #[test]
    fn parse_params_helper() {
        let v = json!({"key": "main"});
        let p: PatchParams = parse_params(v).unwrap();
        assert_eq!(p.key, "main");
    }

    #[test]
    fn parse_params_error() {
        let v = json!({"not_key": true});
        let err = parse_params::<PatchParams>(v).unwrap_err();
        assert!(err.to_string().contains("key"));
    }
}
