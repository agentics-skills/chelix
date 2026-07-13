# Session Tools

Session tools enable persistent, asynchronous coordination between agent
sessions.

## Available Tools

### `sessions_explore`

List all agents that can be passed to `sessions_create`.

Input:

```json
{}
```

Output includes each agent `id`, `name`, `description`, optional persona fields,
and preset model configuration. Use the returned `id` as `agent_id`.

### `sessions_create`

Create a new chat session for one explicit agent. The tool always generates a
standard `session:<uuid>` key and returns it in the result for later calls.

Input:

```json
{
  "agent_id": "required agent id from sessions_explore",
  "label": "optional label",
  "project_id": "optional project id",
  "model_override": {
    "model": "advanced base model id override from models.list",
    "reasoning_effort": "none|minimal|low|medium|high|xhigh|max"
  }
}
```

`agent_id` is mandatory. The tool does not apply an implicit default agent and
does not fall back to another agent if the requested agent is missing.

Omit `model_override` to use the selected agent's preset model. `model_override`
is for advanced intentional overrides only. When it is provided, both
`model_override.model` and `model_override.reasoning_effort` are mandatory. The
model must be the base ID shown in the chat model registry (`models.list`) and
must support reasoning. The tool stores `model` and `reasoning_effort` as
separate session fields, for example:

```json
{
  "agent_id": "researcher",
  "model_override": {
    "model": "anthropic::claude-opus-4-5-20251101",
    "reasoning_effort": "high"
  }
}
```

If `model` is omitted, the selected agent must have both
`[agents.presets.<agent_id>].model` and
`[agents.presets.<agent_id>].reasoning_effort` configured. Otherwise the tool
returns an explicit error explaining which preset field is missing.

Sessions created by an agent are automatically linked to the calling session as
children (`parentSessionKey`), so the sessions sidebar renders them nested under
their creator — the same tree mechanism used for forks. Nesting works
recursively: if the created session's agent creates another session, it nests
one level deeper.

The parent link can also be managed via the `session.patch` RPC using the
`parentSessionKey` field (set a new parent or `null` to detach). Cycles and
self-parenting are rejected.

### `sessions_list`

List sessions visible to the current policy.

Input:

```json
{
  "filter": "optional text",
  "limit": 20
}
```

### `sessions_history`

Read message history from a target session.

Input:

```json
{
  "key": "agent:research:main",
  "limit": 20,
  "offset": 0
}
```

### `sessions_search`

Search prior session history for relevant snippets. By default the current
session is excluded when `_session_key` is available in tool context.

```json
{
  "query": "checkpoint rollback",
  "limit": 5,
  "exclude_current": true
}
```

### `sessions_send`

Send a message to another session, optionally waiting for reply.

```json
{
  "key": "agent:coder:main",
  "message": "Please implement JWT middleware",
  "wait_for_reply": true,
  "context": "coordinator",
  "model_override": {
    "model": "anthropic::claude-opus-4-5-20251101",
    "reasoning_effort": "high"
  }
}
```

Omit `model_override` in `sessions_send` to use the target session model.

## Session Access Policy

Configure policy in a preset to control what sessions a sub-agent can access:

```toml
[agents.presets.coordinator]
tools.allow = ["sessions_list", "sessions_history", "sessions_search", "sessions_send", "task_list", "spawn_agent"]
sessions.can_send = true

[agents.presets.observer]
tools.allow = ["sessions_list", "sessions_history", "sessions_search"]
sessions.key_prefix = "agent:research:"
sessions.can_send = false
```

Policy fields:

- `key_prefix`: restrict visibility by session-key prefix
- `allowed_keys`: extra explicit session keys
- `can_send`: controls `sessions_send` (default: `true`)
- `cross_agent`: allow access to sessions owned by other agents (default:
  `false`)

When no policy is configured, all sessions are visible and sendable.

## Coordination Patterns

Use `spawn_agent` when work is short-lived and synchronous. For longer delegated
work, call `spawn_agent` with `nonblocking: true`; it returns a `task_id` while
the sub-agent continues in the background. Use `spawn_status` to check progress,
`spawn_result` to fetch the final output, `spawn_list` to recover task IDs after
context loss, and `cancel_spawn` to stop work that is no longer needed.

Use `active_tools` and `tool_choice` to prevent model drift on small/cheap LLMs.
These controls apply **per agent run** (not per iteration within a run) and are
available on agent presets, `spawn_agent`, and `cron` `agentTurn` payloads.

- `active_tools` filters the tool schemas visible to the agent.
- `tool_choice` controls provider-level tool selection:
  - `auto` — model decides (default).
  - `any` — model must call some tool but chooses which one.
  - `none` — no tools sent; forces text-only output.
  - `tool` + `name` — model must call the named tool.

Supported on Anthropic, OpenAI (Responses and Chat Completions), and
OpenAI-compatible providers.

**Classify-then-generate pattern** — use two `spawn_agent` calls, each with its
own tool controls:

```json
// Turn 1: forced classifier
{
  "task": "Classify whether the reply should be inline, file, or PR.",
  "active_tools": ["classify_destination"],
  "tool_choice": { "type": "tool", "name": "classify_destination" },
  "nonblocking": true
}
// Turn 2: scoped generation (parent reads classifier result, spawns again)
{
  "task": "Generate the report and send it as a document.",
  "active_tools": ["write_file", "send_document"],
  "tool_choice": { "type": "auto" }
}
```

Example preset defaults:

```toml
[agents.presets.destination-router.tool_controls]
active_tools = ["classify_destination"]

[agents.presets.destination-router.tool_controls.tool_choice]
type = "tool"
name = "classify_destination"
```

Example scheduled agent turn:

```json
{
  "kind": "agentTurn",
  "message": "Generate the report and send it as a document.",
  "active_tools": ["write_file", "send_document"],
  "tool_choice": { "type": "any" }
}
```

Use session tools when you need:

- long-lived specialist sessions
- handoffs with durable history
- asynchronous team-style orchestration

Common coordinator flow:

1. `sessions_explore` to choose an explicit `agent_id`
2. `sessions_create` to create worker sessions
3. `sessions_list` to discover existing workers
4. `sessions_search` to find prior related work
5. `sessions_history` to inspect progress
6. `sessions_send` to dispatch next tasks
7. `task_list` to track cross-session work items
