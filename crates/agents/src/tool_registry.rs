use {
    anyhow::Result,
    async_trait::async_trait,
    chelix_config::schema::McpServerId,
    std::{
        collections::{HashMap, HashSet},
        sync::{Arc, Mutex},
    },
    tracing::warn,
};

/// In-context truncation behavior for a tool's results.
///
/// Full outputs are always persisted to disk regardless of this setting;
/// it only controls whether the in-context copy may be truncated. `Off`
/// prevents recursion for tools that intentionally return oversized
/// content (e.g. a Read mode that reads persisted tool results back).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Truncation {
    /// Truncate results above the configured byte budget (default).
    #[default]
    Standard,
    /// Never truncate the in-context copy of this result.
    Off,
}

/// On-disk representation for a tool's complete result.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ToolResultPersistence {
    /// Do not persist this result. Tools that select this must also disable
    /// truncation so an oversized result never loses its full-output pointer.
    Off,
    /// Persist the exact agent-facing result. Strings use `content.txt`;
    /// structured values use `content.json` plus `schema.json`.
    #[default]
    On,
}

/// Agent-callable tool.
#[async_trait]
pub trait AgentTool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> serde_json::Value;
    /// Opportunistic post-start initialization hook.
    async fn warmup(&self) -> Result<()> {
        Ok(())
    }
    /// Truncation behavior for this call's result. Tools can override this
    /// per tool (ignore `params`) or per call (inspect `params`). This is an
    /// internal knob — it is never exposed through the tool's JSON schema.
    fn truncation(&self, _params: &serde_json::Value) -> Truncation {
        Truncation::Standard
    }
    /// Whether to persist this call's complete agent-facing result.
    fn result_persistence(&self, _params: &serde_json::Value) -> ToolResultPersistence {
        ToolResultPersistence::On
    }
    /// Convert the raw implementation/protocol result into the value exposed
    /// to the LLM and persisted as tool context. The default contract exposes
    /// the complete raw value unchanged, including MCP structured results.
    async fn agent_result(
        &self,
        _params: &serde_json::Value,
        raw_result: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        Ok(raw_result.clone())
    }
    async fn execute(&self, params: serde_json::Value) -> Result<serde_json::Value>;
}

/// Where a tool originates from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolSource {
    /// Built-in tool shipped with the binary.
    Builtin,
    /// Tool provided by an MCP server.
    Mcp { server: McpServerId },
}

/// Internal entry pairing a tool with its source metadata.
pub(crate) struct ToolEntry {
    pub(crate) tool: Arc<dyn AgentTool>,
    pub(crate) source: ToolSource,
}

/// A public tool's name and short description for the prompt catalog.
///
/// Unlike [`ToolRegistry::list_schemas`], the catalog ignores lazy schema
/// visibility, so every allowed public tool is always advertised by name.
/// It never carries parameter schemas — those are fetched on demand via
/// `get_tool` in lazy mode, or sent through the API in full mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCatalogEntry {
    pub name: String,
    pub description: String,
}

/// Shared set of tool names whose schemas are visible in lazy registry mode.
///
/// Uses `std::sync::Mutex` (not tokio) because the lock is held for
/// microseconds — just a `HashSet` insert/lookup — and this keeps
/// `list_schemas()` usable from sync contexts.
pub(crate) type LazyVisibleTools = Arc<Mutex<HashSet<String>>>;

/// Registry of available tools for an agent run.
///
/// Tools are stored as `Arc<dyn AgentTool>` so the registry can be cheaply
/// cloned (e.g. for sub-agents that need a filtered copy of the parent's tools).
pub struct ToolRegistry {
    tools: HashMap<String, ToolEntry>,
    /// In lazy mode, only these tool schemas are exposed through `list_schemas()`.
    /// Execution still uses the full `tools` map.
    lazy_visible: Option<LazyVisibleTools>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            lazy_visible: None,
        }
    }

    pub(crate) fn set_lazy_visible(&mut self, visible: LazyVisibleTools) {
        self.lazy_visible = Some(visible);
    }

    /// Register a built-in tool. Warns (and overwrites) on name collision.
    pub fn register(&mut self, tool: Box<dyn AgentTool>) {
        let name = tool.name().to_string();
        let new_source = ToolSource::Builtin;
        if let Some(existing) = self.tools.get(&name) {
            warn!(
                tool = %name,
                old_source = ?existing.source,
                new_source = ?new_source,
                "tool name collision — new registration overwrites existing entry"
            );
        }
        self.tools.insert(name, ToolEntry {
            tool: Arc::from(tool),
            source: new_source,
        });
    }

    /// Register a tool from an MCP server. Warns (and overwrites) on name collision.
    pub fn register_mcp(&mut self, tool: Box<dyn AgentTool>, server: McpServerId) {
        let name = tool.name().to_string();
        let new_source = ToolSource::Mcp { server };
        if let Some(existing) = self.tools.get(&name) {
            warn!(
                tool = %name,
                old_source = ?existing.source,
                new_source = ?new_source,
                "tool name collision — new registration overwrites existing entry"
            );
        }
        self.tools.insert(name, ToolEntry {
            tool: Arc::from(tool),
            source: new_source,
        });
    }

    /// Replace an existing tool by name, preserving its source metadata.
    ///
    /// Returns `true` if an existing tool was replaced, `false` if this was a new entry.
    pub fn replace(&mut self, tool: Box<dyn AgentTool>) -> bool {
        let name = tool.name().to_string();
        let source = self
            .tools
            .get(&name)
            .map(|entry| entry.source.clone())
            .unwrap_or(ToolSource::Builtin);
        self.tools
            .insert(name, ToolEntry {
                tool: Arc::from(tool),
                source,
            })
            .is_some()
    }

    pub fn unregister(&mut self, name: &str) -> bool {
        self.tools.remove(name).is_some()
    }

    /// Remove all MCP-sourced tools. Returns the number of tools removed.
    pub fn unregister_mcp(&mut self) -> usize {
        let before = self.tools.len();
        self.tools
            .retain(|_, entry| !matches!(entry.source, ToolSource::Mcp { .. }));
        before - self.tools.len()
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn AgentTool>> {
        self.tools.get(name).map(|e| Arc::clone(&e.tool))
    }

    pub fn list_schemas(&self) -> Vec<serde_json::Value> {
        let visible = self
            .lazy_visible
            .as_ref()
            .map(|names| names.lock().unwrap_or_else(|e| e.into_inner()));
        let mut schemas: Vec<serde_json::Value> = self
            .tools
            .iter()
            .filter(|(name, _)| {
                visible
                    .as_ref()
                    .is_none_or(|visible| visible.contains(name.as_str()))
            })
            .map(|(_, entry)| entry_to_schema(entry))
            .collect();
        schemas.sort_by(|left, right| {
            let left_name = left
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let right_name = right
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            left_name.cmp(right_name)
        });
        schemas
    }

    pub fn list_schemas_allowed_by<F>(&self, mut predicate: F) -> Vec<serde_json::Value>
    where
        F: FnMut(&str) -> bool,
    {
        self.list_schemas()
            .into_iter()
            .filter(|schema| {
                schema
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(&mut predicate)
            })
            .collect()
    }

    /// Public tools available in the current filtered registry.
    ///
    /// Ignores lazy schema visibility: every tool is returned, including
    /// `get_tool` when the registry is lazy-wrapped.
    /// Sorted by name. Returns only `name` + `description`, never parameter
    /// schemas — it is the discovery catalog, not the API schema list.
    #[must_use]
    pub fn list_catalog(&self) -> Vec<ToolCatalogEntry> {
        let mut catalog: Vec<ToolCatalogEntry> = self
            .tools
            .iter()
            .map(|(name, entry)| ToolCatalogEntry {
                name: name.clone(),
                description: entry.tool.description().to_string(),
            })
            .collect();
        catalog.sort_by(|left, right| left.name.cmp(&right.name));
        catalog
    }

    /// List tool names currently visible through schema discovery.
    pub fn list_names(&self) -> Vec<String> {
        let visible = self
            .lazy_visible
            .as_ref()
            .map(|names| names.lock().unwrap_or_else(|e| e.into_inner()));
        let mut names: Vec<String> = self
            .tools
            .keys()
            .filter(|name| {
                visible
                    .as_ref()
                    .is_none_or(|visible| visible.contains(name.as_str()))
            })
            .cloned()
            .collect();
        names.sort();
        names
    }

    /// Clone the registry, excluding tools whose names start with `prefix`.
    pub fn clone_without_prefix(&self, prefix: &str) -> ToolRegistry {
        let tools = self
            .tools
            .iter()
            .filter(|(name, _)| !name.starts_with(prefix))
            .map(|(name, entry)| {
                (name.clone(), ToolEntry {
                    tool: Arc::clone(&entry.tool),
                    source: entry.source.clone(),
                })
            })
            .collect();
        ToolRegistry {
            tools,
            lazy_visible: self.lazy_visible.clone(),
        }
    }

    /// Clone the registry, excluding all MCP-sourced tools.
    pub fn clone_without_mcp(&self) -> ToolRegistry {
        let tools = self
            .tools
            .iter()
            .filter(|(_, entry)| !matches!(entry.source, ToolSource::Mcp { .. }))
            .map(|(name, entry)| {
                (name.clone(), ToolEntry {
                    tool: Arc::clone(&entry.tool),
                    source: entry.source.clone(),
                })
            })
            .collect();
        ToolRegistry {
            tools,
            lazy_visible: self.lazy_visible.clone(),
        }
    }

    /// Clone the registry, excluding tools whose names are in `exclude`.
    pub fn clone_without(&self, exclude: &[&str]) -> ToolRegistry {
        let tools = self
            .tools
            .iter()
            .filter(|(name, _)| !exclude.contains(&name.as_str()))
            .map(|(name, entry)| {
                (name.clone(), ToolEntry {
                    tool: Arc::clone(&entry.tool),
                    source: entry.source.clone(),
                })
            })
            .collect();
        ToolRegistry {
            tools,
            lazy_visible: self.lazy_visible.clone(),
        }
    }

    /// Clone the registry keeping only tools that match `predicate`.
    pub fn clone_allowed_by<F>(&self, mut predicate: F) -> ToolRegistry
    where
        F: FnMut(&str) -> bool,
    {
        self.clone_allowed_entries(|name, _| predicate(name))
    }

    /// Clone the registry keeping only tools whose name and source metadata match `predicate`.
    pub fn clone_allowed_entries<F>(&self, mut predicate: F) -> ToolRegistry
    where
        F: FnMut(&str, &ToolSource) -> bool,
    {
        let tools = self
            .tools
            .iter()
            .filter(|(name, entry)| predicate(name, &entry.source))
            .map(|(name, entry)| {
                (name.clone(), ToolEntry {
                    tool: Arc::clone(&entry.tool),
                    source: entry.source.clone(),
                })
            })
            .collect();
        ToolRegistry {
            tools,
            lazy_visible: self.lazy_visible.clone(),
        }
    }
}

fn entry_to_schema(e: &ToolEntry) -> serde_json::Value {
    let mut schema = serde_json::json!({
        "name": e.tool.name(),
        "description": e.tool.description(),
        "parameters": e.tool.parameters_schema(),
    });
    match &e.source {
        ToolSource::Builtin => {
            schema["source"] = serde_json::json!("builtin");
        },
        ToolSource::Mcp { server } => {
            schema["source"] = serde_json::json!("mcp");
            schema["mcpServer"] = serde_json::json!(server.as_str());
        },
    }
    schema
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;

    struct DummyTool {
        name: String,
    }

    #[async_trait]
    impl AgentTool for DummyTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "test"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({})
        }

        async fn execute(&self, _params: serde_json::Value) -> Result<serde_json::Value> {
            Ok(serde_json::json!({}))
        }
    }

    #[test]
    fn test_clone_without_prefix() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(DummyTool {
            name: "execute_command".to_string(),
        }));
        registry.register(Box::new(DummyTool {
            name: "read_file".to_string(),
        }));
        registry.register(Box::new(DummyTool {
            name: "mcp__github_search".to_string(),
        }));
        registry.register(Box::new(DummyTool {
            name: "mcp__memory_store".to_string(),
        }));

        let filtered = registry.clone_without_prefix("mcp__");
        assert_eq!(filtered.list_schemas().len(), 2);
        assert!(filtered.get("execute_command").is_some());
        assert!(filtered.get("read_file").is_some());
        assert!(filtered.get("mcp__github_search").is_none());
        assert!(filtered.get("mcp__memory_store").is_none());
    }

    #[test]
    fn test_clone_without_prefix_no_match() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(DummyTool {
            name: "execute_command".to_string(),
        }));
        registry.register(Box::new(DummyTool {
            name: "read_file".to_string(),
        }));

        let filtered = registry.clone_without_prefix("mcp__");
        assert_eq!(filtered.list_schemas().len(), 2);
    }

    #[test]
    fn test_clone_without_mcp() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(DummyTool {
            name: "execute_command".to_string(),
        }));
        registry.register_mcp(
            Box::new(DummyTool {
                name: "mcp__github__search".to_string(),
            }),
            McpServerId::from("github"),
        );
        registry.register_mcp(
            Box::new(DummyTool {
                name: "mcp__memory__store".to_string(),
            }),
            McpServerId::from("memory"),
        );

        let filtered = registry.clone_without_mcp();
        assert_eq!(filtered.list_schemas().len(), 1);
        assert!(filtered.get("execute_command").is_some());
        assert!(filtered.get("mcp__github__search").is_none());
        assert!(filtered.get("mcp__memory__store").is_none());
    }

    #[test]
    fn test_unregister_mcp() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(DummyTool {
            name: "execute_command".to_string(),
        }));
        registry.register_mcp(
            Box::new(DummyTool {
                name: "mcp__github__search".to_string(),
            }),
            McpServerId::from("github"),
        );
        registry.register_mcp(
            Box::new(DummyTool {
                name: "mcp__memory__store".to_string(),
            }),
            McpServerId::from("memory"),
        );

        let removed = registry.unregister_mcp();
        assert_eq!(removed, 2);
        assert_eq!(registry.list_schemas().len(), 1);
        assert!(registry.get("execute_command").is_some());
    }

    #[test]
    fn test_list_schemas_includes_source() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(DummyTool {
            name: "execute_command".to_string(),
        }));
        registry.register_mcp(
            Box::new(DummyTool {
                name: "mcp__github__search".to_string(),
            }),
            McpServerId::from("github"),
        );

        let schemas = registry.list_schemas();
        let builtin = schemas
            .iter()
            .find(|s| s["name"] == "execute_command")
            .expect("execute_command should exist");
        assert_eq!(builtin["source"], "builtin");
        assert!(builtin.get("mcpServer").is_none() || builtin["mcpServer"].is_null());

        let mcp = schemas
            .iter()
            .find(|s| s["name"] == "mcp__github__search")
            .expect("mcp tool should exist");
        assert_eq!(mcp["source"], "mcp");
        assert_eq!(mcp["mcpServer"], "github");
    }

    #[test]
    fn test_list_names() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(DummyTool {
            name: "execute_command".to_string(),
        }));
        registry.register(Box::new(DummyTool {
            name: "read_file".to_string(),
        }));

        let names = registry.list_names();
        assert_eq!(names, vec![
            "execute_command".to_string(),
            "read_file".to_string()
        ]);
    }

    #[test]
    fn test_list_schemas_are_sorted_by_name() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(DummyTool {
            name: "zeta".to_string(),
        }));
        registry.register(Box::new(DummyTool {
            name: "alpha".to_string(),
        }));
        registry.register(Box::new(DummyTool {
            name: "mu".to_string(),
        }));

        let names: Vec<String> = registry
            .list_schemas()
            .into_iter()
            .filter_map(|schema| {
                schema
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .map(ToString::to_string)
            })
            .collect();

        assert_eq!(names, vec!["alpha", "mu", "zeta"]);
    }

    #[test]
    fn test_get_returns_cloned_tool_handle() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(DummyTool {
            name: "execute_command".to_string(),
        }));
        assert!(registry.get("execute_command").is_some());
        assert!(registry.get("missing").is_none());
    }

    #[test]
    fn test_register_collision_overwrites_with_warning() {
        // The warn! output is emitted via tracing; we assert the overwrite
        // semantics and trust the log at runtime.
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(DummyTool {
            name: "Read".to_string(),
        }));
        // Same name again — should overwrite, warn logged.
        registry.register(Box::new(DummyTool {
            name: "Read".to_string(),
        }));
        assert_eq!(registry.list_names(), vec!["Read".to_string()]);
    }

    #[test]
    fn test_register_mcp_overwriting_builtin_warns() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(DummyTool {
            name: "Read".to_string(),
        }));
        registry.register_mcp(
            Box::new(DummyTool {
                name: "Read".to_string(),
            }),
            McpServerId::from("filesystem"),
        );
        // Source should now be Mcp even though the builtin was registered first.
        let schema = registry
            .list_schemas()
            .into_iter()
            .find(|schema| schema.get("name").and_then(serde_json::Value::as_str) == Some("Read"))
            .unwrap();
        assert_eq!(
            schema.get("source").and_then(serde_json::Value::as_str),
            Some("mcp")
        );
    }

    #[test]
    fn test_list_catalog_sorted_name_and_description_only() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(DummyTool {
            name: "zeta".to_string(),
        }));
        registry.register(Box::new(DummyTool {
            name: "alpha".to_string(),
        }));

        let catalog = registry.list_catalog();
        assert_eq!(catalog, vec![
            ToolCatalogEntry {
                name: "alpha".to_string(),
                description: "test".to_string(),
            },
            ToolCatalogEntry {
                name: "zeta".to_string(),
                description: "test".to_string(),
            },
        ]);
    }

    #[test]
    fn test_list_catalog_ignores_lazy_visibility() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(DummyTool {
            name: "execute_command".to_string(),
        }));
        registry.register(Box::new(DummyTool {
            name: "read_file".to_string(),
        }));
        // Restrict lazy schema visibility to a single tool.
        let visible: LazyVisibleTools =
            Arc::new(Mutex::new(HashSet::from(["execute_command".to_string()])));
        registry.set_lazy_visible(visible);

        // Schemas honor the lazy gate…
        assert_eq!(registry.list_schemas().len(), 1);
        // …but the catalog advertises every public tool regardless.
        let names: Vec<String> = registry
            .list_catalog()
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        assert_eq!(names, vec![
            "execute_command".to_string(),
            "read_file".to_string()
        ]);
    }

    #[test]
    fn test_clone_allowed_by() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(DummyTool {
            name: "execute_command".to_string(),
        }));
        registry.register(Box::new(DummyTool {
            name: "read_file".to_string(),
        }));
        registry.register(Box::new(DummyTool {
            name: "session_state".to_string(),
        }));

        let filtered =
            registry.clone_allowed_by(|name| name.starts_with("read") || name == "execute_command");
        let names = filtered.list_names();
        assert_eq!(names, vec![
            "execute_command".to_string(),
            "read_file".to_string()
        ]);
    }
}
