# Agents

Agents define the model, prompts, tool policy, session access, MCP access, and
skill visibility used by chat and delegated child sessions. Each agent has one
TOML entry and one workspace directory.

## First-Run Setup

Chelix does not create incomplete starter agents. A first-run configuration has
no agent entries and no default agent. After provider setup exposes a registered
model, onboarding requires the user to select that canonical model and one of
its supported reasoning efforts before it creates the first agent and makes it
the default.

## Configuration

`[agents] default` selects the agent for new sessions. Every other key directly
under `[agents]` is an agent ID.

```toml
[agents]
default = "main"

[agents.main]
name = "Chelix"
emoji = "🤖"
description = "General-purpose assistant"
model = "openai::gpt-5.2"
reasoning_effort = "high"
max_tools_threshold = 128
compaction_reminder = true
prepend_sender_badge = true

[agents.main.tools]
allow = []
deny = []
preload = ["read_file", "list_directory", "ripgrep"]
```

Agent IDs are used by session metadata, the chat selector, and
`sub_agent.action.run.agent_id`. An ID must contain lowercase ASCII letters,
numbers, or hyphens; it cannot start or end with a hyphen and cannot exceed 80
bytes. `default` is reserved.

Agent create, update, delete, and default-selection changes take effect without
a gateway restart.

## Prompt Files

Each agent has two system-prompt files:

```text
<data_dir>/agents/<id>/SOUL.md
<data_dir>/agents/<id>/SUBAGENT.md
```

- `SOUL.md` is loaded for sessions with `prompt_profile = "chat"`.
- `SUBAGENT.md` is loaded for sessions with `prompt_profile = "subagent"`.

The UI edits both files under **Settings → Agents**. Chelix does not substitute
`SOUL.md` when `SUBAGENT.md` is empty.

An agent workspace can also contain `AGENTS.md`, `TOOLS.md`, and `MEMORY.md`.
Agent-specific `AGENTS.md` and `TOOLS.md` fall back to the corresponding root
workspace file when absent.

## Delegation

Call `sub_agent` with `explore` before selecting an agent:

```json
{
  "action": {
    "explore": {}
  }
}
```

An agent is listed only when `SUBAGENT.md` is non-empty after trimming. Every
configured agent already has a required model/reasoning pair validated through
the live model registry.

Start a delegated child session with the exact agent ID returned by `explore`:

```json
{
  "action": {
    "run": {
      "agent_id": "reviewer",
      "task": "Review the current changes.",
      "mode": "blocking"
    }
  }
}
```

`mode` must be `blocking` or `background`. See [Sub-Agent
Delegation](sub-agent.md) for all actions and response fields.

## Agent Fields

Each `[agents.<id>]` table supports:

- `name`;
- `emoji`;
- `description`;
- `voice_persona_id`;
- `model`;
- `tools.allow`, `tools.deny`, and `tools.preload`;
- `max_tools_threshold`;
- `compaction_reminder`;
- `prepend_sender_badge`;
- `timeout_secs`;
- `max_tool_result_bytes`;
- `sessions.key_prefix`, `sessions.allowed_keys`, `sessions.can_send`, and
  `sessions.cross_agent`;
- `reasoning_effort`;
- `mcp.allow_servers` or `mcp.deny_servers`;
- `skills.allow` and `skills.deny`.

`name`, `model`, `reasoning_effort`, `max_tools_threshold`,
`compaction_reminder`, and `prepend_sender_badge` are required. The model must use its canonical registry
ID, and the effort must be supported by that model. Unknown fields are rejected.

When `prepend_sender_badge` is true, messages sent via `sessions_send` and
`sub_agent` run or send start with `[From the "<name>" agent]` followed by a blank
line, using the sender agent name.

## Tool Policy

`tools.allow` is a whitelist when non-empty. `tools.deny` removes tools after
the allow list is applied. `tools.preload` exposes selected schemas in lazy
registry mode but does not grant access to a filtered tool.

```toml
[agents.research.tools]
allow = ["read_file", "list_directory", "ripgrep"]
deny = ["execute_command", "overwrite_file"]
preload = ["read_file", "list_directory", "ripgrep"]
```

See [Tool Policy](tool-policy.md) for policy layering.

## Session Access

The optional `sessions` table controls session-tool access:

```toml
[agents.coordinator.sessions]
key_prefix = "agent:"
allowed_keys = []
can_send = true
cross_agent = true
```

See [Session Tools](session-tools.md) for the session APIs.

## MCP and Skills

MCP allow and deny lists are mutually exclusive:

```toml
[agents.research.mcp]
allow_servers = ["github", "memory"]
```

An empty `allow_servers = []` blocks every MCP server for the agent. Skill
visibility is configured independently:

```toml
[agents.research.skills]
allow = ["research", "code-review"]
deny = ["social-media"]
```
