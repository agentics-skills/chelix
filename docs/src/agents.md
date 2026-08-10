# Agents

Chelix uses the same user-owned agents for chat sessions and delegated
`spawn_agent` runs. Every agent has one TOML configuration and one workspace
directory.

## First Run

On first run, Chelix writes these starter agents into the user-owned
`chelix.toml`:

- `main`
- `research`
- `coder`
- `reviewer`
- `qa`
- `ux`
- `docs`
- `coordinator`

Their workspace files are created under `<data_dir>/agents/<id>/`. After this
initial materialization, every starter agent is managed exactly like an agent
created in the UI: it can be edited, selected as the default, or deleted. The
current default agent must be changed before it can be deleted.

## Configuration

`[agents] default` selects the agent used for new sessions and for
`spawn_agent` calls that omit `agent`. Every other key directly under
`[agents]` is an agent ID.

```toml
[agents]
default = "main"

[agents.main]
name = "Chelix"
emoji = "🤖"
description = "General-purpose assistant"
model = "openai/gpt-5.2"
max_tools_threshold = 128

[agents.main.tools]
allow = []
deny = []
preload = ["read_file", "list_directory", "ripgrep"]
```

Agent IDs are used by session metadata, the chat selector, and the
`spawn_agent.agent` parameter. IDs must contain only lowercase ASCII letters,
numbers, and hyphens, must not start or end with a hyphen, and are limited to
80 bytes. The ID `default` is reserved by the static `[agents] default` field
and cannot be used as an agent ID.

Agent create, update, delete, and default-selection changes are applied to chat
and `spawn_agent` immediately; restarting the gateway is not required.

## Prompt Files

Each agent has two independent prompt files:

```text
<data_dir>/agents/<id>/SOUL.md
<data_dir>/agents/<id>/SUBAGENT.md
```

- `SOUL.md` is loaded for normal chat sessions using the agent.
- `SUBAGENT.md` is loaded when the same agent is selected by `spawn_agent`.

Both files are editable next to the structural settings in **Settings →
Agents**. The UI labels the second field **Sub-Agent system prompt**.

An empty `SUBAGENT.md` stays empty. Chelix adds the delegated task and optional
context, but does not substitute a hidden role prompt.

Each agent can also override `AGENTS.md` and `TOOLS.md` in its own workspace.
When either file is absent, all agents use the corresponding root workspace
file. No agent ID receives additional file fallback rules.

## Spawning an Agent

Select any configured agent with the `agent` parameter:

```json
{
  "task": "Find all authentication-related code paths",
  "context": "Return file paths and a concise synthesis.",
  "agent": "research"
}
```

If `agent` is omitted, `spawn_agent` uses `[agents] default`. A missing agent ID
is an error. Model selection order is:

1. Explicit `spawn_agent.model`
2. The selected agent's `model`
3. The parent/default provider model

`spawn_agent` is always excluded from the child tool registry.

## Agent Fields

Each `[agents.<id>]` table supports:

- `name` (required)
- `emoji`
- `description`
- `voice_persona_id`
- `model`
- `max_tools_threshold` (required, at least `1`)
- `timeout_secs`
- `max_tool_result_bytes`
- `reasoning_effort`
- `tools.allow`, `tools.deny`, `tools.preload`
- `tool_controls.active_tools`, `tool_controls.tool_choice`
- `sessions.key_prefix`, `sessions.allowed_keys`, `sessions.can_send`,
  `sessions.cross_agent`
- `memory.scope`, `memory.max_lines`
- `mcp.allow_servers` or `mcp.deny_servers`
- `skills.allow`, `skills.deny`

Unknown fields are rejected.

## Tool Policy

`tools.allow` is an optional whitelist. `tools.deny` removes tools after the
allow-list is applied. `tools.preload` exposes selected schemas when the global
tool registry uses lazy loading; it does not grant access to a filtered tool.

```toml
[agents.research.tools]
allow = ["read_file", "list_directory", "ripgrep"]
deny = ["execute_command", "overwrite_file"]
preload = ["read_file", "list_directory", "ripgrep"]
```

See [Tool Policy](tool-policy.md) for policy layering.

## Session Access

The optional `sessions` table controls session tools for the agent:

```toml
[agents.coordinator.sessions]
key_prefix = "agent:"
allowed_keys = []
can_send = true
cross_agent = true
```

See [Session Tools](session-tools.md) for the session APIs.

## Per-Agent Memory

The optional `memory` table configures memory loaded for spawned runs:

```toml
[agents.research.memory]
scope = "project"
max_lines = 100
```

Supported scopes are:

- `user`: `<data_dir>/agent-memory/<id>/MEMORY.md`
- `project`: `.chelix/agent-memory/<id>/MEMORY.md`
- `local`: `.chelix/agent-memory-local/<id>/MEMORY.md`

For chat prompts, each agent's prompt-visible memory is read from
`<data_dir>/agents/<id>/MEMORY.md`.

## MCP and Skills

MCP allow and deny lists are mutually exclusive:

```toml
[agents.research.mcp]
allow_servers = ["github", "memory"]
```

An empty `allow_servers = []` blocks all MCP servers for that agent. Skill
visibility can be restricted independently:

```toml
[agents.research.skills]
allow = ["research", "code-review"]
deny = ["social-media"]
```
