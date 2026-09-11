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

Output includes each agent `id`, `name`, `description`, optional display fields,
and model configuration. Use the returned `id` as `agent_id`.

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

`agent_id` is mandatory. Accepted input fields are `agent_id`, `label`,
`project_id`, and `model_override`. Additional fields are rejected before session
creation. String fields must be non-empty; omit unused optional fields rather
than passing `null`.

Omit `model_override` to use the selected agent's configured model. `model_override`
is for advanced intentional overrides only. When it is provided, both
`model_override.model` and `model_override.reasoning_effort` are mandatory. The
model must be the base ID shown in the chat model registry (`models.list`) and
must support the selected effort. The override accepts only `model` and
`reasoning_effort`. The tool stores the validated pair atomically, for example:

```json
{
  "agent_id": "researcher",
  "model_override": {
    "model": "openai::gpt-5.2",
    "reasoning_effort": "high"
  }
}
```

When `model_override` is omitted, the tool uses the selected agent's required
model/reasoning pair. Agent pairs are validated against the live model registry
at startup and whenever an agent is created or updated.

Sessions created by an agent receive the calling session from the typed execution
context and are automatically linked to it as children (`parentSessionKey`), so
the sessions sidebar renders them nested under
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

Read semantic message snapshots from a target session through
`UiHistoryEngine` (`crates/tools/src/sessions_communicate.rs`). `offset` skips
newest messages; the returned `messages` are in conversation order. The result
includes `totalMessages`, `count`, `hasMore`, `generation`, and `revision`.
Snapshots merge persisted state with active, accumulated provider/tool input and
carry stable `id`, `position`, and `revision` fields. Assistant snapshots carry
complete text, visible reasoning, tool calls, provider item identities and usage
metadata. Raw API debug payloads (`llmApiResponse`) belong to the canonical journal;
`UiSnapshot` uses a separate typed assistant serialization view for semantic
history (`crates/sessions/src/ui_history_serialization.rs`).

Input:

```json
{
  "key": "agent:research:main",
  "limit": 20,
  "offset": 0
}
```

### `sessions_search`

Search the public semantic content of prior sessions, including active
snapshots and UI presentations. By default the current session is excluded
when `_session_key` is available in tool context. Session metadata and access
policy restrict candidate sessions before the result limit is applied.

A session with a nonempty canonical journal but no UI history row is excluded
from cross-session search; the server logs a warning with its session key. Such
sessions remain in the session list, with preview backfill skipped. Direct history
reads and opening that conversation refuse service with the session key in the
error. Explicit session clearing or deletion remains available. Runtime history
failures and database/I/O errors still propagate rather than being skipped.

Each result carries `messageId`, `generation`, `position`, `role`, and `snippet`,
plus its session metadata. UI search navigation loads the exact message ID in
that generation (`crates/web/ui/src/session-search.ts`).

```json
{
  "query": "API design decisions",
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
  "model": {
    "session": {}
  }
}
```

```json
{
  "key": "agent:coder:main",
  "message": "Please implement JWT middleware",
  "wait_for_reply": true,
  "context": "coordinator",
  "model": {
    "override": {
      "model": "openai::gpt-5.2",
      "reasoning_effort": "high"
    }
  }
}
```

`key` and `message` are required non-empty strings. Accepted input fields are
`key`, `message`, `wait_for_reply`, `context`, and `model`. Additional
fields are rejected before reading session state or sending a message.
`wait_for_reply` is a boolean and defaults to `false` when omitted. Optional
`context` must be a non-empty string when supplied. Omit unused optional fields;
explicit `null` is rejected.

`model` is required and selects exactly one form. `model.session` is an empty
object and uses the target session's persisted model/reasoning pair.
`model.override` accepts only the required non-empty `model` and
`reasoning_effort` fields and is validated against the model registry.

When the sender agent has `prepend_sender_badge = true`, the delivered message
starts with `[From the "<name>" agent]` followed by a blank line, using the
sender agent name. The badge is placed above the optional `context` prefix.

## Session Access Policy

Configure policy on an agent to control which sessions it can access:

```toml
[agents.coordinator]
name = "Coordinator"
model = "openai::gpt-5.2"
reasoning_effort = "medium"
max_tools_threshold = 128
compaction_reminder = true
prepend_sender_badge = true
tools.allow = ["sessions_list", "sessions_history", "sessions_search", "sessions_send", "task_list", "sub_agent"]
sessions.can_send = true

[agents.observer]
name = "Observer"
model = "openai::gpt-5.2"
reasoning_effort = "medium"
max_tools_threshold = 128
compaction_reminder = true
prepend_sender_badge = true
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

Use `sub_agent` for delegated work. `run` with `mode = "blocking"` returns the
child response directly. `mode = "background"` returns a child session key for
`status`, `result`, or `cancel`; `list` returns every direct child of the calling
session. The parent session for `run`, `status`, `list`, `result`, and `cancel`
comes from the typed execution context; these actions require that context.
`explore` can execute independently. The public input accepts exactly one
`action` with its closed parameter object, checked before invoking the action.
See [Sub-Agent Delegation](sub-agent.md) for the action schemas.

Use `toolChoice` as a top-level request parameter for `chat.send` and
`chat.send_sync`, or in a `cron` `agentTurn` payload, to control provider-level
tool selection:

- `auto` — model decides.
- `any` — model must call a tool.
- `none` — no tools are sent.
- `tool` + `name` — model must call the named tool.

OpenAI Responses, OpenAI Chat Completions, and OpenAI-compatible providers
support `toolChoice`.

Example direct chat request:

```json
{
  "text": "Generate the report in a file.",
  "toolChoice": { "type": "any" }
}
```

Example scheduled agent turn:

```json
{
  "kind": "agentTurn",
  "message": "Generate the report in a file.",
  "toolChoice": { "type": "any" }
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

### Delegated Child Sessions

Use [`sub_agent`](sub-agent.md) to delegate a bounded task to a persisted direct
child session. Blocking mode returns the child response from the `run` call.
Background mode returns a child session key and run ID; use `status`, `result`,
or `cancel` only from the direct parent session. `list` returns that parent's
direct children.

The child stores its direct parent, selected agent, prompt profile, and resolved
sandbox owner. General session visibility and messaging remain controlled by the
session tools and session access policy described above.
