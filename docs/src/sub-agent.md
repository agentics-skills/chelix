# Sub-Agent Delegation

The unified `sub_agent` tool creates and coordinates delegated work in persisted
direct-child sessions. It supports six actions: `explore`, `run`, `status`,
`list`, `result`, and `cancel`.

The parameter schema is one strict root object with one required `action`
property. The `action` object contains exactly one action name, and that name
contains the strict parameter object for the selected action. Every object is
closed with `additionalProperties: false`. Unknown parameters are rejected with
the offending field name. No action has default parameter values.

## Explore

`explore` has no parameters:

```json
{
  "action": {
    "explore": {}
  }
}
```

The response contains `agents`; each entry contains `id`, `name`, and
`description`.

An agent is available only when all of these conditions are met:

1. `<data_dir>/agents/<id>/SUBAGENT.md` is non-empty after trimming;
2. `[agents.<id>].model` is configured;
3. `[agents.<id>].reasoning_effort` is configured.

`sub_agent` does not resolve the configured model or validate provider support
for the configured reasoning effort. The chat service resolves the model and the
provider applies the reasoning effort for each session turn.

## Run

`run` requires three parameters:

- `agent_id`: a non-empty ID returned by `explore`;
- `task`: a non-empty string;
- `mode`: either `"blocking"` or `"background"`.

The same availability criteria used by `explore` are checked again for
`agent_id`. The tool does not accept per-run model, reasoning, context,
tool-control, label, project, timeout, or depth parameters.

The parent session key comes from tool context. The server creates a
`session:<uuid>` child, persists its direct parent, selected agent, agent model,
`prompt_profile = "subagent"`, and resolved sandbox owner, and generates the
label from the agent ID and truncated task. The complete first child message is
`task`. The child receives the full tool registry; the selected agent and normal
tool-policy layers determine tool access.

Blocking mode sends the child turn synchronously. Its result contains
`sessionKey`, `agentId`, `mode`, `text`, `inputTokens`, `outputTokens`, and
`durationMs`, transferred from the synchronous chat response.

Background mode dispatches the child turn asynchronously. Its result contains
`sessionKey`, `agentId`, `mode`, `runId`, and `status: "running"`. The tool keeps
the background `sessionKey` to `runId` association for `result`.

## Status and List

`status` requires `session_key`. The target session must have
`parent_session_key` equal to the calling session key. The response contains
`sessionKey`, `agentId`, `label`, `status`, `messageCount`, `createdAt`, and
`updatedAt`. `status` is `"running"` when the chat service reports an active run
and `"idle"` otherwise.

`list` has no parameters and returns every session whose `parent_session_key`
equals the calling session key. Each entry uses the `status` response format.

## Result

`result` requires `session_key` and applies the same direct-parent ownership
check as `status`. An active run is an explicit error.

For a completed tracked background run, `result` reads messages for the stored
`runId` and returns `sessionKey`, `agentId`, and `text`. `text` is the content of
the last assistant message from that run.

Blocking output is returned directly by `run`; `result` returns completed
background output.

## Cancel

`cancel` requires `session_key` and applies the same direct-parent ownership
check as `status`. It aborts the child session's background chat run without
deleting the session or its history.

The response contains `sessionKey`, `aborted`, and `runId`, transferred from the
chat abort response. An active run returns `aborted = true`; a completed run
returns `aborted = false`.

## Prompt Profile

Every session has a persisted `prompt_profile`:

- `chat` loads `SOUL.md`;
- `subagent` loads `SUBAGENT.md`.

A delegated child uses `prompt_profile = "subagent"`. An empty `SUBAGENT.md` at
run time is an explicit error; `SOUL.md` is never substituted. All other prompt
assembly is shared with a normal session.

## Shared Sandbox

A delegated child persists its parent's already resolved sandbox owner. Nested
delegation keeps the same root owner, so the parent and descendants use one
sandbox container. Cleanup requested for a non-owner child is a no-op. Session
ownership checks keep terminals created by one session inaccessible to the other
sessions in the shared container.

A referenced sandbox owner that does not exist is an explicit routing error and
does not fall back to the child's own key.

## Observability

Public asynchronous operations use tracing instrumentation. Session creation
records the selected agent, mode, and sandbox owner; blocking completion is also
recorded.

When metrics are enabled, delegated runs emit
`chelix_sub_agent_runs_total{mode,status}` and
`chelix_sub_agent_run_duration_seconds{mode}`.
