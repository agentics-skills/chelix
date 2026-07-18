# Tool Registry

The tool registry manages all tools available to the agent during a
conversation. It tracks where each tool comes from and supports filtering by
source.

## Tool Sources

Every registered tool has a `ToolSource` that identifies its origin:

- **`Builtin`** — tools shipped with the binary (`execute_command`, `web_fetch`,
  etc.)
- **`Mcp { server }`** — tools provided by an MCP server, tagged with the server
  name

This replaces the previous convention of identifying MCP tools by their `mcp__`
name prefix, providing type-safe filtering instead of string matching.

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
  "name": "execute_command",
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

The `source` and `mcpServer` fields are available to the UI for rendering tools
grouped by origin.

## Command execution tools

Chelix registers command execution tools for agent command work:

- `execute_command` runs through the active execution route: local host, paired
  node, SSH target, or isolated sandbox. Isolated sandbox runs paste a
  structured command into a real tmux pane. It accepts `command`, `customCwd`,
  `newTerminal`, `destructiveFlag`, `background`, `timeout`, optional `node`,
  and an optional `terminalId` for reusing a managed terminal.
- `read_terminal_output` captures output from a managed tmux terminal by
  `terminalId`. Use it after a foreground timeout or for background commands
  that continue running after `execute_command` returns.

When a sandbox has no tmux server yet, `execute_command` creates a tmux session
and returns the generated `terminalId`, tmux session/window/pane IDs, output,
completion state, and exit code when available.

## Ripgrep tool

The `ripgrep` tool searches files by shelling out to the system `rg` binary
with `--json` output and returns structured results. The binary is assumed to
be installed — a spawn failure surfaces as a tool error.

Parameters (camelCase): `pattern` (required), `paths`, `cwd`, `fixedStrings`,
`caseMode` (`sensitive`/`ignore`/`smart`), `detail` (`summary`, `files`,
`lines` — default, `lines+submatches`), `glob`, `type`, `typeNot`,
`contextLines`, `maxMatches` (2000), `maxFiles` (200), `maxOutputChars`
(200000), `timeoutMs` (10000), `includeHidden` (default `true`),
`unrestricted` (0–3, default 3, maps to `-u`/`-uu`/`-uuu`), `followSymlinks`.

Common extension-like `type` values (`tsx`, `jsx`, `mjs`, …) are normalized to
rg type names; unknown extension-like values become glob filters; anything
else is passed to rg verbatim so rg itself rejects unknown types.

Exceeding a match/file/output limit or the timeout stops the search early,
kills the rg process, and marks the result `truncated` with a
`truncatedReason` (`maxMatches`, `maxFiles`, `maxOutputChars`, `timeout`).
The result mirrors the limits, a summary (`filesWithMatches`, `matchCount`,
`elapsed`, `stats`), rows per detail mode, captured `stderr`, and the rg
`exitCode`. Exit code 2 (for example an invalid regex) is a tool error.

## Catalog vs API schemas

The registry exposes two independent surfaces:

- **`list_catalog()`** — every allowed **public** tool as a
  `{ name, description }` pair, sorted by name. It ignores lazy schema
  visibility, so the discovery catalog is always complete. Tools whose names end
  in `_wasm` are execution-only and never appear here.
- **`list_schemas()`** — the full JSON parameter schemas, filtered by lazy
  visibility. This is what is sent to the provider as the API tool list (native
  mode) or embedded in the prompt (text mode).

The system prompt's **`## Available Tools`** section is built from
`list_catalog()` and lists every allowed tool by a JSON-name label so the
identifier is unambiguous:

```text
## Available Tools

- `{"name":"Edit"}`: Exact-match string replacement in a file...
- `{"name":"Glob"}`: Find files matching a glob pattern...
- `{"name":"get_tool"}`: Fetch the full parameter schema...
```

This format is identical in native and text mode, and in the live, debug, and UI
prompt surfaces. In text mode the parameter schemas follow in a separate
**`## Tool Schemas`** block (headings use the same `{"name":"<tool>"}` label),
because text mode can't send schemas through the provider API.

## Lazy Registry Mode

By default every LLM turn includes full JSON schemas for all registered tools.
With many MCP servers this can burn 15,000+ tokens per turn. **Lazy mode** keeps
the full catalog advertised but defers the parameter schemas: only the
`get_tool` meta-tool and schemas the model has fetched by exact name are sent.

### Configuration

```toml
[tools]
registry_mode = "lazy"   # default: "full"
```

### How it works

1. `Available Tools` still lists every allowed tool by name (the full catalog),
   plus `get_tool`. Only `get_tool`'s parameter schema is sent initially.
2. `get_tool(name="memory_search")` returns that tool's full schema and makes it
   visible. `get_tool` takes exactly one argument, `name` — an exact tool name
   from `Available Tools`. There is no keyword search, and any other field is
   rejected. An unknown name returns a structured `schema_visible: false`
   response rather than an execution error.
3. `get_tool(name="get_tool")` is a valid lookup that returns the meta-tool's
   own schema.
4. Allowed tools remain executable before their schema is revealed. Once the
   model knows the exact tool name and parameters, it should call the tool
   directly — standard pipeline, hooks fire normally. `get_tool` is not an
   execution permission step and should not be repeated for the same tool.

The runner re-computes schemas each iteration, so revealed schemas appear
immediately. On later turns, lazy visibility is restored from structured session
history: prior successful `get_tool` schema reveals (`tool_result` with
`tool_name == "get_tool"`, `success == true`, and
`result.schema_visible == true`) and prior assistant tool calls keep those
schemas visible. The restoration is not inferred from user or assistant prose,
and older sessions that predate `get_tool` simply start from `{get_tool}`. The
iteration limit is tripled in lazy mode to account for the extra discovery
round-trips.

`get_tool` is a reserved control-plane name: enabling lazy mode fails cleanly if
a user or MCP tool is already named `get_tool`, and the existing tool is left
untouched.

### When to use

- Many MCP servers connected (50+ tools)
- Long conversations where input token cost matters
- Sub-agent runs that only need a few specific tools

In **full** mode (default), all schemas are sent every turn — no behavioral
change from before this feature.
