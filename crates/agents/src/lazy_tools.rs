//! Lazy tool registry: the model fetches tool schemas via `get_tool` instead
//! of receiving every schema upfront.
//!
//! When `registry_mode = "lazy"` is set in config, [`wrap_registry_lazy`]
//! keeps every allowed tool executable but hides their JSON parameter schemas
//! from the API/prompt until the model reveals them by exact name. The full
//! catalog of allowed public tool names is always advertised in
//! `Available Tools`; only the parameter schemas are deferred. The model calls
//! `get_tool(name="tool_name")` once to reveal a tool's schema, then calls that
//! tool directly.

use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

use {anyhow::Result, async_trait::async_trait, tracing::debug};

use crate::tool_registry::{AgentTool, LazyVisibleTools, ToolRegistry};

/// Reserved control-plane meta-tool name. A user or MCP tool may not use it.
pub const GET_TOOL_NAME: &str = "get_tool";

const GET_TOOL_DESCRIPTION: &str = concat!(
    "Fetch the full parameter schema for one allowed tool by exact name. ",
    "Call once per tool when you need its schema, then call that tool directly.",
);

/// The control-plane schema for `get_tool` itself. Single source of truth used
/// for both registration and the reveal snapshot, so `get_tool(name="get_tool")`
/// is a valid exact lookup.
fn get_tool_parameters_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "name": {
                "type": "string",
                "description": "Exact tool name from Available Tools."
            }
        },
        "required": ["name"],
        "additionalProperties": false
    })
}

/// A single tool's schema in the lazy snapshot: exact name, description, and
/// parameter schema. Execution stays in the registry; this is reveal-only.
#[derive(Clone)]
struct ToolSchemaEntry {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

impl ToolSchemaEntry {
    fn from_schema(schema: serde_json::Value) -> Option<Self> {
        Some(Self {
            name: schema.get("name")?.as_str()?.to_string(),
            description: schema.get("description")?.as_str()?.to_string(),
            parameters: schema
                .get("parameters")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({})),
        })
    }
}

/// Control-plane meta-tool: reveals one allowed tool's parameter schema by
/// exact name so the model can then call that tool directly.
pub struct GetTool {
    /// Reveal-only snapshot of allowed public tool schemas, including
    /// `get_tool` itself. Execution remains in the registry.
    entries: Arc<Vec<ToolSchemaEntry>>,
    /// Shared visible-name set used by the wrapper registry's `list_schemas()`.
    visible: LazyVisibleTools,
}

impl GetTool {
    /// Build the reveal-only snapshot from the source registry's public
    /// schemas, plus the `get_tool` schema itself. Called before the meta-tool
    /// is registered so the snapshot reflects the underlying tools only.
    fn build_entries(registry: &ToolRegistry) -> Vec<ToolSchemaEntry> {
        let mut entries: Vec<ToolSchemaEntry> = registry
            .list_schemas()
            .into_iter()
            .filter_map(ToolSchemaEntry::from_schema)
            .collect();
        entries.push(ToolSchemaEntry {
            name: GET_TOOL_NAME.to_string(),
            description: GET_TOOL_DESCRIPTION.to_string(),
            parameters: get_tool_parameters_schema(),
        });
        entries
    }

    fn reveal_tool_schema(&self, name: &str) -> serde_json::Value {
        let Some(entry) = self.entries.iter().find(|entry| entry.name == name) else {
            return unknown_tool_response(name);
        };

        let mut visible = self.visible.lock().unwrap_or_else(|e| e.into_inner());
        visible.insert(name.to_string());

        debug!(tool = name, "tool schema revealed via get_tool");

        serde_json::json!({
            "schema_visible": true,
            "name": name,
            "description": entry.description.clone(),
            "parameters": entry.parameters.clone(),
            "hint": format!("Schema for `{name}` is now visible. Call `{name}` directly; do not call get_tool again for it.")
        })
    }
}

fn unknown_tool_response(name: &str) -> serde_json::Value {
    serde_json::json!({
        "schema_visible": false,
        "name": name,
        "error": format!("unknown tool: {name}"),
        "hint": "`name` must be an exact tool name from Available Tools. Skills are not tools."
    })
}

#[async_trait]
impl AgentTool for GetTool {
    fn name(&self) -> &str {
        GET_TOOL_NAME
    }

    fn description(&self) -> &str {
        GET_TOOL_DESCRIPTION
    }

    fn parameters_schema(&self) -> serde_json::Value {
        get_tool_parameters_schema()
    }

    async fn execute(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        // Strict control-plane contract: exactly one model-supplied key, `name`.
        // Any other model field (including `query`) is rejected as an execution
        // error. Underscore-prefixed keys are runtime-injected context
        // (`_session_key`, `_accept_language`, `_conn_id`, `_channel`, …) that
        // the runner merges into every tool call — they are not model input and
        // are ignored here, matching the `_` convention `public_tool_arguments`
        // uses to strip them.
        let serde_json::Value::Object(fields) = &params else {
            return Err(anyhow::anyhow!(
                "get_tool expects a JSON object with a single `name` field."
            ));
        };
        if let Some(unexpected) = fields
            .keys()
            .find(|key| !key.starts_with('_') && key.as_str() != "name")
        {
            return Err(anyhow::anyhow!(
                "get_tool accepts only `name`; unexpected field `{unexpected}`."
            ));
        }
        let Some(name_value) = fields.get("name") else {
            return Err(anyhow::anyhow!("get_tool requires a `name` field."));
        };
        let Some(name) = name_value.as_str() else {
            return Err(anyhow::anyhow!("get_tool `name` must be a string."));
        };
        let name = name.trim();
        if name.is_empty() {
            return Err(anyhow::anyhow!("get_tool `name` must not be empty."));
        }
        Ok(self.reveal_tool_schema(name))
    }
}

/// Wrap a full tool registry for lazy mode.
///
/// Returns the same executable registry, but with schema visibility restricted
/// to `get_tool` until individual schemas are revealed by exact name.
///
/// Fails if the source registry already contains a tool named `get_tool`: the
/// reserved control-plane name must not be shadowed, and the existing tool is
/// left untouched.
pub fn wrap_registry_lazy(registry: ToolRegistry) -> Result<ToolRegistry> {
    wrap_registry_lazy_with_visible(registry, std::iter::empty::<String>())
}

/// Wrap a full tool registry for lazy mode and restore already-visible schemas.
///
/// `visible_names` is intersected with the current public registry, so only
/// real public tools become visible; `get_tool` is always visible.
pub fn wrap_registry_lazy_with_visible<I, S>(
    mut registry: ToolRegistry,
    visible_names: I,
) -> Result<ToolRegistry>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    if registry.get(GET_TOOL_NAME).is_some() {
        return Err(anyhow::anyhow!(
            "cannot enable lazy registry mode: a tool named `{GET_TOOL_NAME}` is already registered"
        ));
    }

    // Snapshot public schemas and public names before the lazy gate is set,
    // while `list_schemas()`/`list_names()` still return the full set.
    let entries = Arc::new(GetTool::build_entries(&registry));
    let public_names: HashSet<String> = registry.list_names().into_iter().collect();

    let mut visible_set = HashSet::from([GET_TOOL_NAME.to_string()]);
    visible_set.extend(
        visible_names
            .into_iter()
            .map(Into::into)
            .filter(|name| public_names.contains(name)),
    );
    let visible = Arc::new(Mutex::new(visible_set));

    registry.set_lazy_visible(Arc::clone(&visible));
    registry.register(Box::new(GetTool { entries, visible }));
    Ok(registry)
}

/// Reconstruct lazy-visible tool schemas from persisted chat history.
///
/// Only the current persisted wire format is supported:
/// - assistant `tool_calls` restore direct public tool names;
/// - a `tool_result` counts only when `tool_name == "get_tool"`, `success ==
///   true`, and the top-level `result.schema_visible == true`, taking the
///   revealed name from `result.name`.
///
/// `get_tool` is removed from the returned set because the wrapper always adds
/// it. Legacy formats (old meta-tool name, `activated`, nested/JSON-string
/// payloads) are intentionally not restored — old lazy sessions start from
/// `{get_tool}`.
pub fn visible_tool_names_from_history(history: &[serde_json::Value]) -> HashSet<String> {
    let mut visible = HashSet::new();

    for message in history {
        match message.get("role").and_then(serde_json::Value::as_str) {
            Some("assistant") => collect_direct_tool_calls(message, &mut visible),
            Some("tool_result") => collect_get_tool_reveal(message, &mut visible),
            _ => {},
        }
    }

    visible.remove(GET_TOOL_NAME);
    visible
}

fn collect_direct_tool_calls(message: &serde_json::Value, visible: &mut HashSet<String>) {
    let Some(tool_calls) = message
        .get("tool_calls")
        .and_then(serde_json::Value::as_array)
    else {
        return;
    };

    for tool_call in tool_calls {
        let Some(name) = tool_call
            .get("function")
            .and_then(|function| function.get("name"))
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty() && *name != GET_TOOL_NAME)
        else {
            continue;
        };
        visible.insert(name.to_string());
    }
}

fn collect_get_tool_reveal(message: &serde_json::Value, visible: &mut HashSet<String>) {
    if message.get("tool_name").and_then(serde_json::Value::as_str) != Some(GET_TOOL_NAME) {
        return;
    }
    if message.get("success").and_then(serde_json::Value::as_bool) != Some(true) {
        return;
    }
    let Some(result) = message.get("result") else {
        return;
    };
    if result
        .get("schema_visible")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
    {
        return;
    }
    if let Some(name) = result
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        visible.insert(name.to_string());
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;

    struct DummyTool {
        tool_name: String,
        tool_desc: String,
    }

    impl DummyTool {
        fn new(name: &str, desc: &str) -> Self {
            Self {
                tool_name: name.to_string(),
                tool_desc: desc.to_string(),
            }
        }
    }

    #[async_trait]
    impl AgentTool for DummyTool {
        fn name(&self) -> &str {
            &self.tool_name
        }

        fn description(&self) -> &str {
            &self.tool_desc
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "input": { "type": "string" }
                }
            })
        }

        async fn execute(&self, _params: serde_json::Value) -> Result<serde_json::Value> {
            Ok(serde_json::json!({ "ok": true }))
        }
    }

    fn build_full_registry() -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(DummyTool::new(
            "execute_command",
            "Execute a shell command",
        )));
        registry.register(Box::new(DummyTool::new(
            "ripgrep",
            "Search workspace files",
        )));
        registry.register(Box::new(DummyTool::new(
            "memory_search",
            "Search long-term memory for relevant information",
        )));
        registry.register(Box::new(DummyTool::new(
            "memory_save",
            "Save information to long-term memory",
        )));
        registry
    }

    // ── Catalog / schema surfaces after wrap ─────────────────────────

    #[test]
    fn wrap_registry_lazy_catalog_lists_public_tools_plus_get_tool() {
        let full = build_full_registry();
        let lazy = wrap_registry_lazy(full).unwrap();

        let names: Vec<String> = lazy
            .list_catalog()
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        assert_eq!(names, vec![
            "execute_command".to_string(),
            GET_TOOL_NAME.to_string(),
            "memory_save".to_string(),
            "memory_search".to_string(),
            "ripgrep".to_string(),
        ]);
    }

    #[test]
    fn wrap_registry_lazy_schemas_contain_only_get_tool() {
        let full = build_full_registry();
        let lazy = wrap_registry_lazy(full).unwrap();

        let names = lazy.list_names();
        assert_eq!(names, vec![GET_TOOL_NAME.to_string()]);
        assert_eq!(lazy.list_schemas().len(), 1);
    }

    #[tokio::test]
    async fn revealing_a_tool_adds_only_it_and_get_tool_to_schemas() {
        let full = build_full_registry();
        let lazy = wrap_registry_lazy(full).unwrap();

        // Before schema reveal, only get_tool is visible, but allowed tools are executable.
        assert_eq!(lazy.list_schemas().len(), 1);
        assert!(lazy.get("execute_command").is_some());

        let get_tool = lazy.get(GET_TOOL_NAME).unwrap();
        let result = get_tool
            .execute(serde_json::json!({ "name": "execute_command" }))
            .await
            .unwrap();

        assert_eq!(result["schema_visible"], true);
        assert_eq!(result["name"], "execute_command");
        assert!(result["parameters"].is_object());

        let names = lazy.list_names();
        assert_eq!(names, vec![
            "execute_command".to_string(),
            GET_TOOL_NAME.to_string()
        ]);
    }

    #[test]
    fn wrap_registry_lazy_rejects_reserved_get_tool_collision() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(DummyTool::new(GET_TOOL_NAME, "user tool")));
        registry.register(Box::new(DummyTool::new("ripgrep", "Search files")));

        let err = wrap_registry_lazy(registry).err().unwrap();
        assert!(err.to_string().contains(GET_TOOL_NAME));
    }

    #[test]
    fn wrap_registry_lazy_collision_does_not_wrap() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(DummyTool::new(GET_TOOL_NAME, "user tool")));

        assert!(wrap_registry_lazy_with_visible(registry, ["ripgrep".to_string()]).is_err());
    }

    #[test]
    fn wrap_registry_lazy_with_visible_restores_schema_visibility() {
        let full = build_full_registry();
        let lazy = wrap_registry_lazy_with_visible(full, ["execute_command".to_string()]).unwrap();

        let names = lazy.list_names();
        assert_eq!(names, vec![
            "execute_command".to_string(),
            GET_TOOL_NAME.to_string()
        ]);
        assert!(lazy.get("memory_search").is_some());
    }

    #[test]
    fn wrap_registry_lazy_with_visible_ignores_unknown_names() {
        let registry = build_full_registry();
        let lazy = wrap_registry_lazy_with_visible(registry, [
            "execute_command".to_string(),
            "does_not_exist".to_string(),
        ])
        .unwrap();

        // Only the existing tool (plus get_tool) becomes schema-visible.
        assert_eq!(lazy.list_names(), vec![
            "execute_command".to_string(),
            GET_TOOL_NAME.to_string()
        ]);
    }

    // ── GetTool contract ─────────────────────────────────────────────

    #[test]
    fn get_tool_parameters_schema_is_strict_single_name() {
        let full = build_full_registry();
        let lazy = wrap_registry_lazy(full).unwrap();
        let get_tool = lazy.get(GET_TOOL_NAME).unwrap();

        let schema = get_tool.parameters_schema();
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["required"], serde_json::json!(["name"]));
        let props = schema["properties"].as_object().unwrap();
        assert_eq!(props.len(), 1);
        assert!(props.contains_key("name"));
    }

    #[tokio::test]
    async fn get_tool_reveals_known_name() {
        let full = build_full_registry();
        let lazy = wrap_registry_lazy(full).unwrap();
        let get_tool = lazy.get(GET_TOOL_NAME).unwrap();

        let result = get_tool
            .execute(serde_json::json!({ "name": "memory_search" }))
            .await
            .unwrap();
        assert_eq!(result["schema_visible"], true);
        assert_eq!(result["name"], "memory_search");
        assert!(result["parameters"].is_object());
        assert!(
            result["hint"]
                .as_str()
                .unwrap()
                .contains("Call `memory_search` directly")
        );
    }

    #[tokio::test]
    async fn get_tool_trims_external_whitespace_before_exact_match() {
        let full = build_full_registry();
        let lazy = wrap_registry_lazy(full).unwrap();
        let get_tool = lazy.get(GET_TOOL_NAME).unwrap();

        let result = get_tool
            .execute(serde_json::json!({ "name": "  memory_search  " }))
            .await
            .unwrap();
        assert_eq!(result["schema_visible"], true);
        assert_eq!(result["name"], "memory_search");
    }

    #[tokio::test]
    async fn get_tool_returns_own_schema_for_self_lookup() {
        let full = build_full_registry();
        let lazy = wrap_registry_lazy(full).unwrap();
        let get_tool = lazy.get(GET_TOOL_NAME).unwrap();

        let result = get_tool
            .execute(serde_json::json!({ "name": GET_TOOL_NAME }))
            .await
            .unwrap();
        assert_eq!(result["schema_visible"], true);
        assert_eq!(result["name"], GET_TOOL_NAME);
        assert_eq!(result["parameters"]["additionalProperties"], false);
    }

    #[tokio::test]
    async fn get_tool_rejects_empty_name() {
        let full = build_full_registry();
        let lazy = wrap_registry_lazy(full).unwrap();
        let get_tool = lazy.get(GET_TOOL_NAME).unwrap();

        assert!(
            get_tool
                .execute(serde_json::json!({ "name": "   " }))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn get_tool_rejects_missing_name() {
        let full = build_full_registry();
        let lazy = wrap_registry_lazy(full).unwrap();
        let get_tool = lazy.get(GET_TOOL_NAME).unwrap();

        assert!(get_tool.execute(serde_json::json!({})).await.is_err());
    }

    #[tokio::test]
    async fn get_tool_rejects_non_string_name() {
        let full = build_full_registry();
        let lazy = wrap_registry_lazy(full).unwrap();
        let get_tool = lazy.get(GET_TOOL_NAME).unwrap();

        assert!(
            get_tool
                .execute(serde_json::json!({ "name": 42 }))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn get_tool_rejects_extra_field() {
        let full = build_full_registry();
        let lazy = wrap_registry_lazy(full).unwrap();
        let get_tool = lazy.get(GET_TOOL_NAME).unwrap();

        let err = get_tool
            .execute(serde_json::json!({ "name": "memory_search", "query": "memory" }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("query"));
    }

    #[tokio::test]
    async fn get_tool_ignores_runtime_injected_context_params() {
        let full = build_full_registry();
        let lazy = wrap_registry_lazy(full).unwrap();
        let get_tool = lazy.get(GET_TOOL_NAME).unwrap();

        // The runner merges tool_context into every tool call's params. These
        // underscore-prefixed keys are not model input and must not be rejected.
        let result = get_tool
            .execute(serde_json::json!({
                "name": "memory_search",
                "_session_key": "session:abc",
                "_accept_language": "en-US,en;q=0.9",
                "_conn_id": "c3a84311",
                "_channel": { "surface": "web" },
            }))
            .await
            .unwrap();

        assert_eq!(result["schema_visible"], true);
        assert_eq!(result["name"], "memory_search");
    }

    #[tokio::test]
    async fn get_tool_still_rejects_model_field_alongside_context_params() {
        let full = build_full_registry();
        let lazy = wrap_registry_lazy(full).unwrap();
        let get_tool = lazy.get(GET_TOOL_NAME).unwrap();

        // Service params are tolerated, but a stray model-supplied field is not.
        let err = get_tool
            .execute(serde_json::json!({
                "name": "memory_search",
                "_session_key": "session:abc",
                "query": "memory",
            }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("query"));
    }

    #[tokio::test]
    async fn get_tool_context_params_alone_still_require_name() {
        let full = build_full_registry();
        let lazy = wrap_registry_lazy(full).unwrap();
        let get_tool = lazy.get(GET_TOOL_NAME).unwrap();

        assert!(
            get_tool
                .execute(serde_json::json!({ "_session_key": "session:abc" }))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn get_tool_query_only_is_rejected() {
        let full = build_full_registry();
        let lazy = wrap_registry_lazy(full).unwrap();
        let get_tool = lazy.get(GET_TOOL_NAME).unwrap();

        assert!(
            get_tool
                .execute(serde_json::json!({ "query": "memory" }))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn get_tool_unknown_name_is_structured_without_suggestions() {
        let full = build_full_registry();
        let lazy = wrap_registry_lazy(full).unwrap();
        let get_tool = lazy.get(GET_TOOL_NAME).unwrap();

        let result = get_tool
            .execute(serde_json::json!({ "name": "nonexistent" }))
            .await
            .unwrap();
        assert_eq!(result["schema_visible"], false);
        assert_eq!(result["name"], "nonexistent");
        assert_eq!(result["error"], "unknown tool: nonexistent");
        assert!(
            result["hint"]
                .as_str()
                .unwrap()
                .contains("Skills are not tools")
        );
        assert!(result.get("suggestions").is_none());
        assert!(result.get("mcpServers").is_none());
        assert!(result.get("results").is_none());
    }

    #[tokio::test]
    async fn allowed_tool_is_executable_before_schema_reveal() {
        let full = build_full_registry();
        let lazy = wrap_registry_lazy(full).unwrap();

        let command_tool = lazy.get("execute_command").unwrap();
        let result = command_tool
            .execute(serde_json::json!({ "input": "hello" }))
            .await
            .unwrap();
        assert_eq!(result["ok"], true);
    }

    // ── History restore ──────────────────────────────────────────────

    #[test]
    fn history_restores_successful_get_tool_reveal() {
        let history = vec![serde_json::json!({
            "role": "tool_result",
            "tool_name": GET_TOOL_NAME,
            "success": true,
            "result": {
                "schema_visible": true,
                "name": "Glob"
            }
        })];

        let visible = visible_tool_names_from_history(&history);
        assert!(visible.contains("Glob"));
        assert!(!visible.contains(GET_TOOL_NAME));
    }

    #[test]
    fn history_ignores_failed_get_tool_reveal() {
        let history = vec![serde_json::json!({
            "role": "tool_result",
            "tool_name": GET_TOOL_NAME,
            "success": false,
            "result": {
                "schema_visible": true,
                "name": "Glob"
            }
        })];

        assert!(!visible_tool_names_from_history(&history).contains("Glob"));
    }

    #[test]
    fn history_ignores_schema_not_visible_reveal() {
        let history = vec![serde_json::json!({
            "role": "tool_result",
            "tool_name": GET_TOOL_NAME,
            "success": true,
            "result": {
                "schema_visible": false,
                "name": "Glob"
            }
        })];

        assert!(!visible_tool_names_from_history(&history).contains("Glob"));
    }

    #[test]
    fn history_restores_direct_tool_calls() {
        let history = vec![serde_json::json!({
            "role": "assistant",
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {
                    "name": "Glob",
                    "arguments": "{\"pattern\":\"**/*.rs\"}"
                }
            }]
        })];

        assert!(visible_tool_names_from_history(&history).contains("Glob"));
    }

    #[test]
    fn history_ignores_nested_and_string_results() {
        // Nested `result.result` payloads and JSON-string results are legacy
        // formats and are no longer restored.
        let nested = vec![serde_json::json!({
            "role": "tool_result",
            "tool_name": GET_TOOL_NAME,
            "success": true,
            "result": { "result": { "schema_visible": true, "name": "Glob" } }
        })];
        assert!(!visible_tool_names_from_history(&nested).contains("Glob"));

        let stringified = vec![serde_json::json!({
            "role": "tool_result",
            "tool_name": GET_TOOL_NAME,
            "success": true,
            "result": "{\"schema_visible\":true,\"name\":\"Glob\"}"
        })];
        assert!(!visible_tool_names_from_history(&stringified).contains("Glob"));
    }
}
