# Tool Registry

The tool registry manages all tools available to the agent during a
conversation. It tracks where each tool comes from and supports filtering by
source.

## Tool Sources

Every registered tool has a `ToolSource` that identifies its origin:

- **`Builtin`** — tools shipped with the binary (exec, web_fetch, etc.)
- **`Mcp { server }`** — tools provided by an MCP server, tagged with the
  server name

This replaces the previous convention of identifying MCP tools by their
`mcp__` name prefix, providing type-safe filtering instead of string matching.

## Registration

```rust
// Built-in tool
registry.register(Box::new(MyTool::new()));

// MCP tool — tagged with server name
registry.register_mcp(Box::new(adapter), "github".to_string());
```

## Filtering

When MCP tools are disabled for a session, the registry can produce a filtered
copy:

```rust
// Type-safe: filters by ToolSource::Mcp variant
let no_mcp = registry.clone_without_mcp();

// Remove all MCP tools in-place (used during sync)
let removed_count = registry.unregister_mcp();
```

## Schema Output

`list_schemas()` includes source metadata in every tool schema:

```json
{
  "name": "exec",
  "description": "Execute a command",
  "parameters": { ... },
  "source": "builtin"
}
```

```json
{
  "name": "mcp__github__search",
  "description": "Search GitHub",
  "parameters": { ... },
  "source": "mcp",
  "mcpServer": "github"
}
```

The `source` and `mcpServer` fields are available to the UI for rendering
tools grouped by origin.

## Sandbox terminal tools

Chelix registers tmux-backed sandbox terminal tools for agent command work:

- `execute_command` pastes a structured command into a real tmux pane inside
  the current sandbox session. It accepts `command`, `customCwd`,
  `newTerminal`, `destructiveFlag`, `background`, `timeout`, and an optional
  `terminalId` for reusing a managed terminal.
- `read_terminal_output` captures output from a managed tmux terminal by
  `terminalId`. Use it after a foreground timeout or for background commands
  that continue running after `execute_command` returns.

When a sandbox has no tmux server yet, `execute_command` creates a tmux session
and returns the generated `terminalId`, tmux session/window/pane IDs, output,
completion state, and exit code when available.

## Lazy Registry Mode

By default every LLM turn includes full JSON schemas for all registered tools.
With many MCP servers this can burn 15,000+ tokens per turn. **Lazy mode**
replaces all tool schemas with a single `tool_search` meta-tool that the model
uses to discover tool names and inspect schemas on demand.

### Configuration

```toml
[tools]
registry_mode = "lazy"   # default: "full"
```

### How it works

1. The model receives only `tool_search` in its tool list.
2. `tool_search(query="memory")` returns name + description pairs (max 15), no schemas.
3. `tool_search(name="memory_search")` returns the full schema and makes that schema visible.
4. Once the model knows the exact tool name and parameters, it should call `memory_search` directly — standard pipeline, hooks fire normally. `tool_search` is not an execution permission step and should not be repeated for the same known tool.

The runner re-computes schemas each iteration, so revealed schemas appear
immediately. On later turns, lazy visibility is restored from structured session
history: prior successful `tool_search(name=...)` schema reveals and prior
assistant tool calls keep those schemas visible without re-running
`tool_search`. The restoration is not inferred from user or assistant prose. The
iteration limit is tripled in lazy mode to account for the extra discovery
round-trips.

### When to use

- Many MCP servers connected (50+ tools)
- Long conversations where input token cost matters
- Sub-agent runs that only need a few specific tools

In **full** mode (default), all schemas are sent every turn — no behavioral change from before this feature.
