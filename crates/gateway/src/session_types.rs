//! Typed parameter structs for complex session RPC methods.
//!
//! Only methods with non-trivial parameter shapes (multi-field with defaults,
//! null-vs-absent semantics, precedence logic) get dedicated structs here.
//! Simple key-only handlers use inline `.get(...)` directly.

use serde::Deserialize;

use {
    crate::services::ServiceError,
    chelix_common::ReasoningEffort,
    chelix_sessions::{
        metadata::{ToolPermissionMode, ToolPermissionType},
        ui_history_types::UiHistoryTarget,
    },
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
    pub parent_session_key: Option<Option<String>>,
    #[serde(default)]
    pub tool_permission_mode: Option<ToolPermissionMode>,
    #[serde(default)]
    pub tool_permission_type: Option<ToolPermissionType>,
}

impl PatchParams {
    /// Live tool-permission flags must patch during an active turn.
    #[must_use]
    pub(crate) fn is_tool_permission_only(&self) -> bool {
        (self.tool_permission_mode.is_some() || self.tool_permission_type.is_some())
            && self.label.is_none()
            && self.model.is_none()
            && self.reasoning_effort.is_none()
            && self.archived.is_none()
            && self.project_id.is_none()
            && self.worktree_branch.is_none()
            && self.parent_session_key.is_none()
    }

    /// Display label is independent of the active turn.
    #[must_use]
    pub(crate) fn is_label_only(&self) -> bool {
        self.label.is_some()
            && self.model.is_none()
            && self.reasoning_effort.is_none()
            && self.archived.is_none()
            && self.project_id.is_none()
            && self.worktree_branch.is_none()
            && self.parent_session_key.is_none()
            && self.tool_permission_mode.is_none()
            && self.tool_permission_type.is_none()
    }
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
        assert!(matches!(p.parent_session_key, Some(None)));
        assert_eq!(p.tool_permission_mode, None);
        assert_eq!(p.tool_permission_type, None);
    }

    #[test]
    fn patch_params_accepts_tool_permission_fields() {
        let p: PatchParams = serde_json::from_value(json!({
            "key": "main",
            "toolPermissionMode": "moderated",
            "toolPermissionType": "manual",
        }))
        .unwrap();
        assert_eq!(p.tool_permission_mode, Some(ToolPermissionMode::Moderated));
        assert_eq!(p.tool_permission_type, Some(ToolPermissionType::Manual));
        assert!(p.is_tool_permission_only());
    }

    #[test]
    fn patch_params_tool_permission_only_requires_no_other_fields() {
        let mode_only: PatchParams = serde_json::from_value(json!({
            "key": "main",
            "toolPermissionMode": "moderated",
        }))
        .unwrap();
        assert!(mode_only.is_tool_permission_only());

        let type_only: PatchParams = serde_json::from_value(json!({
            "key": "main",
            "toolPermissionType": "manual",
        }))
        .unwrap();
        assert!(type_only.is_tool_permission_only());

        let with_archive: PatchParams = serde_json::from_value(json!({
            "key": "main",
            "toolPermissionMode": "moderated",
            "archived": true,
        }))
        .unwrap();
        assert!(!with_archive.is_tool_permission_only());

        let label_only: PatchParams = serde_json::from_value(json!({
            "key": "main",
            "label": "My Chat",
        }))
        .unwrap();
        assert!(!label_only.is_tool_permission_only());
    }

    #[test]
    fn patch_params_label_only_requires_no_other_fields() {
        let label_only: PatchParams = serde_json::from_value(json!({
            "key": "main",
            "label": "My Chat",
        }))
        .unwrap();
        assert!(label_only.is_label_only());
        assert!(!label_only.is_tool_permission_only());

        let with_archive: PatchParams = serde_json::from_value(json!({
            "key": "main",
            "label": "My Chat",
            "archived": true,
        }))
        .unwrap();
        assert!(!with_archive.is_label_only());

        let with_null_model: PatchParams = serde_json::from_value(json!({
            "key": "main",
            "label": "My Chat",
            "model": null,
        }))
        .unwrap();
        assert_eq!(with_null_model.model, Some(None));
        assert!(!with_null_model.is_label_only());

        let tool_only: PatchParams = serde_json::from_value(json!({
            "key": "main",
            "toolPermissionMode": "moderated",
        }))
        .unwrap();
        assert!(!tool_only.is_label_only());
        assert!(tool_only.is_tool_permission_only());
    }

    #[test]
    fn patch_params_rejects_unknown_tool_permission_mode() {
        let result: Result<PatchParams, _> = serde_json::from_value(json!({
            "key": "main",
            "toolPermissionMode": "unknown",
        }));
        assert!(result.is_err());
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
