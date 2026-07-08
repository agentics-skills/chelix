//! Lazy tool registry: model discovers tool schemas via `tool_search` instead
//! of receiving all schemas upfront.
//!
//! When `registry_mode = "lazy"` is set in config, [`wrap_registry_lazy`]
//! keeps every allowed tool executable but hides their schemas from the prompt
//! until the model discovers them via `tool_search`.
//! The model calls `tool_search(query="…")` to find tool names and
//! `tool_search(name="tool_name")` only when it needs that tool's full schema.

use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

use {anyhow::Result, async_trait::async_trait, tracing::debug};

use crate::tool_registry::{AgentTool, LazyVisibleTools, ToolRegistry};

/// Maximum number of results returned by a keyword search.
const MAX_SEARCH_RESULTS: usize = 15;

#[derive(Clone)]
struct ToolSearchEntry {
    name: String,
    description: String,
    parameters: serde_json::Value,
    source: Option<String>,
    mcp_server: Option<String>,
}

impl ToolSearchEntry {
    fn from_schema(schema: serde_json::Value) -> Option<Self> {
        Some(Self {
            name: schema.get("name")?.as_str()?.to_string(),
            description: schema.get("description")?.as_str()?.to_string(),
            parameters: schema
                .get("parameters")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({})),
            source: schema
                .get("source")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            mcp_server: schema
                .get("mcpServer")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        })
    }
}

/// Meta-tool that lets the model discover and inspect schemas for allowed tools.
pub struct ToolSearchTool {
    /// Search-only snapshot of allowed tool schemas. Execution remains in the registry.
    entries: Arc<Vec<ToolSearchEntry>>,
    /// Shared visible-name set used by the wrapper registry's `list_schemas()`.
    visible: LazyVisibleTools,
}

impl ToolSearchTool {
    fn build_entries(registry: &ToolRegistry) -> Vec<ToolSearchEntry> {
        registry
            .list_schemas()
            .into_iter()
            .filter_map(ToolSearchEntry::from_schema)
            .collect()
    }

    fn keyword_search(&self, query: &str) -> Vec<(String, String, u32)> {
        let query_lower = query.to_lowercase();
        let query_words: Vec<&str> = query_lower.split_whitespace().collect();

        let mut results: Vec<(String, String, u32)> = Vec::new();

        for entry in self.entries.iter() {
            let name_lower = entry.name.to_lowercase();
            let desc_lower = entry.description.to_lowercase();

            let score = if name_lower == query_lower {
                100
            } else if name_lower.contains(&query_lower) {
                50
            } else {
                let word_matches = query_words
                    .iter()
                    .filter(|w| name_lower.contains(*w) || desc_lower.contains(*w))
                    .count();
                if word_matches > 0 {
                    (word_matches as u32) * 10
                } else {
                    0
                }
            };

            if score > 0 {
                results.push((entry.name.clone(), entry.description.clone(), score));
            }
        }

        results.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
        results.truncate(MAX_SEARCH_RESULTS);
        results
    }

    fn reveal_tool_schema(&self, name: &str) -> serde_json::Value {
        let Some(entry) = self.entries.iter().find(|entry| entry.name == name) else {
            return self.unknown_tool_response(name);
        };

        let mut visible = self.visible.lock().unwrap_or_else(|e| e.into_inner());
        visible.insert(name.to_string());

        debug!(tool = name, "tool schema revealed via tool_search");

        serde_json::json!({
            "schema_visible": true,
            "name": name,
            "description": entry.description.clone(),
            "parameters": entry.parameters.clone(),
            "hint": format!("Schema for `{name}` is now visible. Do not call tool_search for `{name}` again; call `{name}` directly when needed.")
        })
    }

    fn unknown_tool_response(&self, name: &str) -> serde_json::Value {
        let suggestions: Vec<serde_json::Value> = self
            .keyword_search(name)
            .into_iter()
            .map(|(name, desc, _score)| {
                serde_json::json!({
                    "name": name,
                    "description": desc
                })
            })
            .collect();
        let mcp_servers: Vec<String> = self
            .entries
            .iter()
            .filter(|entry| entry.source.as_deref() == Some("mcp"))
            .filter_map(|entry| entry.mcp_server.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();

        serde_json::json!({
            "schema_visible": false,
            "name": name,
            "error": format!("unknown tool: {name}"),
            "suggestions": suggestions,
            "mcpServers": mcp_servers,
            "hint": "The `name` field must be an exact tool name from search results or Available Tools. Skills are not tools. For connected MCP servers, search with query set to the server or domain name, then use the exact `mcp__<server>__<tool>` name directly or inspect its schema once with tool_search(name=...)."
        })
    }
}

#[async_trait]
impl AgentTool for ToolSearchTool {
    fn name(&self) -> &str {
        "tool_search"
    }

    fn description(&self) -> &str {
        "Search for available tools by keyword, or inspect a specific tool schema by exact name. \
         Use `query` to find tools (returns name + description, max 15 results). \
            Use `name` only when you need the full parameter schema; if you already know the exact tool name and arguments, call that tool directly instead of calling tool_search again."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Keyword to search tool names and descriptions. Omit this field when using `name`; do not send an empty string."
                },
                "name": {
                    "type": "string",
                    "description": "Exact non-empty tool name to inspect once when its full schema is needed. Omit this field for keyword search; do not send an empty string."
                }
            },
            "additionalProperties": false
        })
    }

    async fn execute(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let query = params
            .get("query")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty());

        match (name, query) {
            (Some(name), _) => {
                // Reveal a specific tool schema by name.
                Ok(self.reveal_tool_schema(name))
            },
            (None, Some(query)) => {
                // Keyword search.
                let results = self.keyword_search(query);
                let items: Vec<serde_json::Value> = results
                    .into_iter()
                    .map(|(name, desc, _score)| {
                        serde_json::json!({
                            "name": name,
                            "description": desc
                        })
                    })
                    .collect();
                Ok(serde_json::json!({
                    "results": items,
                    "hint": "If you already know the required arguments, call the selected tool directly. Use tool_search(name=...) only once when you need the full schema."
                }))
            },
            (None, None) => Err(anyhow::anyhow!(
                "Provide either `name` (to inspect a tool schema) or `query` (to search)."
            )),
        }
    }
}

/// Wrap a full tool registry for lazy mode.
///
/// Returns the same executable registry, but with schema visibility restricted
/// to `tool_search` until individual schemas are revealed by exact name.
pub fn wrap_registry_lazy(registry: ToolRegistry) -> ToolRegistry {
    wrap_registry_lazy_with_visible(registry, std::iter::empty::<String>())
}

/// Wrap a full tool registry for lazy mode and restore already visible schemas.
pub fn wrap_registry_lazy_with_visible<I, S>(
    mut registry: ToolRegistry,
    visible_names: I,
) -> ToolRegistry
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let entries = Arc::new(ToolSearchTool::build_entries(&registry));
    let mut visible_set = HashSet::from(["tool_search".to_string()]);
    visible_set.extend(visible_names.into_iter().map(Into::into));
    let visible = Arc::new(Mutex::new(visible_set));

    registry.set_lazy_visible(Arc::clone(&visible));
    registry.register(Box::new(ToolSearchTool { entries, visible }));
    registry
}

/// Reconstruct lazy-visible tool schemas from persisted chat history.
pub fn visible_tool_names_from_history(history: &[serde_json::Value]) -> HashSet<String> {
    let mut visible = HashSet::new();

    for message in history {
        match message.get("role").and_then(serde_json::Value::as_str) {
            Some("assistant") => collect_direct_tool_calls(message, &mut visible),
            Some("tool_result") => collect_tool_search_reveal(message, &mut visible),
            _ => {},
        }
    }

    visible.remove("tool_search");
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
            .filter(|name| !name.is_empty() && *name != "tool_search")
        else {
            continue;
        };
        visible.insert(name.to_string());
    }
}

fn collect_tool_search_reveal(message: &serde_json::Value, visible: &mut HashSet<String>) {
    if message.get("tool_name").and_then(serde_json::Value::as_str) != Some("tool_search") {
        return;
    }
    if message
        .get("success")
        .and_then(serde_json::Value::as_bool)
        .is_some_and(|success| !success)
    {
        return;
    }
    let Some(result) = message.get("result") else {
        return;
    };
    if let Some(name) = revealed_name_from_tool_search_result(result) {
        visible.insert(name);
    }
}

fn revealed_name_from_tool_search_result(result: &serde_json::Value) -> Option<String> {
    if let Some(name) = revealed_name_from_tool_search_result_object(result) {
        return Some(name);
    }
    if let Some(inner) = result.get("result")
        && let Some(name) = revealed_name_from_tool_search_result_object(inner)
    {
        return Some(name);
    }
    if let Some(text) = result.as_str()
        && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(text)
    {
        return revealed_name_from_tool_search_result(&parsed);
    }
    None
}

fn revealed_name_from_tool_search_result_object(result: &serde_json::Value) -> Option<String> {
    let schema_visible = result
        .get("schema_visible")
        .or_else(|| result.get("activated"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if !schema_visible {
        return None;
    }
    result
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
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
            "web_fetch",
            "Fetch a URL and return its content",
        )));
        registry.register(Box::new(DummyTool::new(
            "memory_search",
            "Search long-term memory for relevant information",
        )));
        registry.register(Box::new(DummyTool::new(
            "memory_save",
            "Save information to long-term memory",
        )));
        registry.register(Box::new(DummyTool::new(
            "memory_forget",
            "Forget information from long-term memory using natural language",
        )));
        registry.register(Box::new(DummyTool::new(
            "memory_delete",
            "Delete information from long-term memory",
        )));
        registry.register(Box::new(DummyTool::new(
            "browser_navigate",
            "Navigate browser to a URL",
        )));
        registry.register_mcp(
            Box::new(DummyTool::new(
                "mcp__vmcp-ha__homeassistant_ha_get_state",
                "Get current status, state, and attributes from Home Assistant",
            )),
            chelix_config::schema::McpServerId::from("vmcp-ha"),
        );
        registry
    }

    #[test]
    fn wrap_registry_lazy_contains_only_tool_search() {
        let full = build_full_registry();
        assert_eq!(full.list_names().len(), 8);

        let lazy = wrap_registry_lazy(full);
        let names = lazy.list_names();
        assert_eq!(names.len(), 1);
        assert!(names.contains(&"tool_search".to_string()));
    }

    #[tokio::test]
    async fn keyword_search_returns_matching_tools() {
        let full = build_full_registry();
        let lazy = wrap_registry_lazy(full);
        let search_tool = lazy.get("tool_search").unwrap();

        let result = search_tool
            .execute(serde_json::json!({ "query": "memory" }))
            .await
            .unwrap();

        let results = result["results"].as_array().unwrap();
        assert_eq!(results.len(), 4);
        let names: Vec<&str> = results
            .iter()
            .map(|r| r["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"memory_search"));
        assert!(names.contains(&"memory_save"));
        assert!(names.contains(&"memory_forget"));
        assert!(names.contains(&"memory_delete"));
    }

    #[tokio::test]
    async fn keyword_search_by_description() {
        let full = build_full_registry();
        let lazy = wrap_registry_lazy(full);
        let search_tool = lazy.get("tool_search").unwrap();

        let result = search_tool
            .execute(serde_json::json!({ "query": "shell" }))
            .await
            .unwrap();

        let results = result["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["name"], "execute_command");
    }

    #[tokio::test]
    async fn empty_name_with_query_performs_keyword_search() {
        let full = build_full_registry();
        let lazy = wrap_registry_lazy(full);
        let search_tool = lazy.get("tool_search").unwrap();

        let result = search_tool
            .execute(serde_json::json!({ "name": "", "query": "shell" }))
            .await
            .unwrap();

        assert!(result.get("error").is_none());
        let results = result["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["name"], "execute_command");
    }

    #[tokio::test]
    async fn blank_name_and_blank_query_return_error() {
        let full = build_full_registry();
        let lazy = wrap_registry_lazy(full);
        let search_tool = lazy.get("tool_search").unwrap();

        let result = search_tool
            .execute(serde_json::json!({ "name": "  ", "query": "" }))
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Provide either"));
    }

    #[tokio::test]
    async fn reveal_tool_schema_does_not_gate_execution() {
        let full = build_full_registry();
        let lazy = wrap_registry_lazy(full);

        // Before schema reveal, only tool_search is visible, but allowed tools are executable.
        assert_eq!(lazy.list_schemas().len(), 1);
        assert!(lazy.get("execute_command").is_some());

        let search_tool = lazy.get("tool_search").unwrap();
        let result = search_tool
            .execute(serde_json::json!({ "name": "execute_command" }))
            .await
            .unwrap();

        assert_eq!(result["schema_visible"], true);
        assert_eq!(result["name"], "execute_command");
        assert!(result["parameters"].is_object());

        // After schema reveal, execute_command's schema is visible.
        assert_eq!(lazy.list_schemas().len(), 2);
        assert!(lazy.get("execute_command").is_some());
    }

    #[test]
    fn wrap_registry_lazy_with_visible_restores_schema_visibility() {
        let full = build_full_registry();
        let lazy = wrap_registry_lazy_with_visible(full, ["execute_command".to_string()]);

        let names = lazy.list_names();
        assert_eq!(names, vec![
            "execute_command".to_string(),
            "tool_search".to_string()
        ]);
        assert!(lazy.get("memory_search").is_some());
    }

    #[test]
    fn visible_tool_names_from_history_tracks_successful_schema_reveals() {
        let history = vec![serde_json::json!({
            "role": "tool_result",
            "tool_name": "tool_search",
            "success": true,
            "result": {
                "schema_visible": true,
                "name": "Glob"
            }
        })];

        let visible = visible_tool_names_from_history(&history);
        assert!(visible.contains("Glob"));
    }

    #[test]
    fn visible_tool_names_from_history_tracks_direct_tool_calls() {
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

        let visible = visible_tool_names_from_history(&history);
        assert!(visible.contains("Glob"));
    }

    #[test]
    fn visible_tool_names_from_history_ignores_failed_schema_reveals() {
        let history = vec![serde_json::json!({
            "role": "tool_result",
            "tool_name": "tool_search",
            "success": false,
            "result": {
                "schema_visible": true,
                "name": "Glob"
            }
        })];

        let visible = visible_tool_names_from_history(&history);
        assert!(!visible.contains("Glob"));
    }

    #[tokio::test]
    async fn unknown_tool_schema_request_returns_recovery_response() {
        let full = build_full_registry();
        let lazy = wrap_registry_lazy(full);
        let search_tool = lazy.get("tool_search").unwrap();

        let result = search_tool
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
    }

    #[tokio::test]
    async fn unknown_tool_schema_request_returns_search_suggestions() {
        let full = build_full_registry();
        let lazy = wrap_registry_lazy(full);
        let search_tool = lazy.get("tool_search").unwrap();

        let result = search_tool
            .execute(serde_json::json!({ "name": "memory" }))
            .await
            .unwrap();

        let suggestions = result["suggestions"].as_array().unwrap();
        let names: Vec<&str> = suggestions
            .iter()
            .map(|r| r["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"memory_search"));
        assert!(names.contains(&"memory_save"));
    }

    #[tokio::test]
    async fn skill_name_schema_request_gives_mcp_recovery_hint() {
        let full = build_full_registry();
        let lazy = wrap_registry_lazy(full);
        let search_tool = lazy.get("tool_search").unwrap();

        let result = search_tool
            .execute(serde_json::json!({ "name": "mcporter", "query": "null" }))
            .await
            .unwrap();

        assert_eq!(result["schema_visible"], false);
        assert_eq!(result["error"], "unknown tool: mcporter");
        assert_eq!(result["mcpServers"], serde_json::json!(["vmcp-ha"]));
        assert!(
            result["hint"]
                .as_str()
                .unwrap()
                .contains("mcp__<server>__<tool>")
        );
    }

    #[tokio::test]
    async fn search_finds_homeassistant_mcp_tools() {
        let full = build_full_registry();
        let lazy = wrap_registry_lazy(full);
        let search_tool = lazy.get("tool_search").unwrap();

        let result = search_tool
            .execute(serde_json::json!({ "query": "homeassistant" }))
            .await
            .unwrap();

        let names: Vec<&str> = result["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"mcp__vmcp-ha__homeassistant_ha_get_state"));
    }

    #[tokio::test]
    async fn no_params_returns_error() {
        let full = build_full_registry();
        let lazy = wrap_registry_lazy(full);
        let search_tool = lazy.get("tool_search").unwrap();

        let result = search_tool.execute(serde_json::json!({})).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Provide either"));
    }

    #[tokio::test]
    async fn name_takes_priority_over_query() {
        let full = build_full_registry();
        let lazy = wrap_registry_lazy(full);
        let search_tool = lazy.get("tool_search").unwrap();

        // When both name and query are provided, name (schema lookup) takes priority.
        let result = search_tool
            .execute(serde_json::json!({ "name": "execute_command", "query": "memory" }))
            .await
            .unwrap();

        assert_eq!(result["schema_visible"], true);
        assert_eq!(result["name"], "execute_command");
    }

    #[tokio::test]
    async fn search_results_capped_at_max() {
        let mut registry = ToolRegistry::new();
        for i in 0..20 {
            registry.register(Box::new(DummyTool::new(
                &format!("tool_{i}"),
                "a matching description",
            )));
        }

        let lazy = wrap_registry_lazy(registry);
        let search_tool = lazy.get("tool_search").unwrap();
        let result = search_tool
            .execute(serde_json::json!({ "query": "matching" }))
            .await
            .unwrap();
        assert!(result["results"].as_array().unwrap().len() <= MAX_SEARCH_RESULTS);
    }

    #[tokio::test]
    async fn search_no_match_returns_empty() {
        let full = build_full_registry();
        let lazy = wrap_registry_lazy(full);
        let search_tool = lazy.get("tool_search").unwrap();

        let result = search_tool
            .execute(serde_json::json!({ "query": "zzzznonexistent" }))
            .await
            .unwrap();

        let results = result["results"].as_array().unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn allowed_tool_is_executable_before_schema_reveal() {
        let full = build_full_registry();
        let lazy = wrap_registry_lazy(full);

        // Tool execution is not gated by lazy schema visibility.
        let command_tool = lazy.get("execute_command").unwrap();
        let result = command_tool
            .execute(serde_json::json!({ "input": "hello" }))
            .await
            .unwrap();

        assert_eq!(result["ok"], true);
    }
}
