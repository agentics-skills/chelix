# Sub-Agent Delegation

`sub_agent` delegates work to persisted direct-child sessions. It has six
actions: `explore`, `run`, `status`, `list`, `result`, and `cancel`.

The root object requires `action`. `action` must contain exactly one action name
and its parameter object. Unknown fields are rejected. No parameter has a
default value.

## Explore

```json
{
  "action": {
    "explore": {}
  }
}
```

The response contains `agents`. Each entry contains `id`, `name`, and
`description`.

An agent is available only when:

1. `<data_dir>/agents/<id>/SUBAGENT.md` is non-empty after trimming;
2. `[agents.<id>].model` is configured;
3. `[agents.<id>].reasoning_effort` is configured.

## Run

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

`run` requires:

- `agent_id`: an available agent ID;
- `task`: a non-empty string;
- `mode`: `blocking` or `background`.

The availability checks run again before the child session is created. `run`
does not accept model, reasoning, context, tool-control, label, project,
timeout, or depth overrides.

The child receives a generated `session:<uuid>` key, the caller as its direct
parent, the selected agent, the agent's model and reasoning effort,
`prompt_profile = "subagent"`, and the parent's resolved sandbox owner. Its
first user message is the exact `task` value. Normal tool-policy layers control
its access to the shared tool registry.

A blocking response contains:

- `sessionKey`;
- `agentId`;
- `mode`;
- `text`;
- `inputTokens`;
- `outputTokens`;
- `durationMs`.

A background response contains:

- `sessionKey`;
- `agentId`;
- `mode`;
- `runId`;
- `status: "running"`.

## Status

```json
{
  "action": {
    "status": {
      "session_key": "session:<uuid>"
    }
  }
}
```

The target must be a direct child of the calling session. The response contains
`sessionKey`, `agentId`, `label`, `status`, `messageCount`, `createdAt`, and
`updatedAt`. `status` is `running` while the chat service reports an active run;
otherwise it is `idle`.

## List

```json
{
  "action": {
    "list": {}
  }
}
```

The response contains `sessions` with every direct child of the calling
session. Each entry has the status response fields.

## Result

```json
{
  "action": {
    "result": {
      "session_key": "session:<uuid>"
    }
  }
}
```

`result` accepts only a completed background run owned by the calling session.
It returns `sessionKey`, `agentId`, and the final assistant `text` from the
tracked run. A running child is an error. Blocking output is returned directly
by `run`.

## Cancel

```json
{
  "action": {
    "cancel": {
      "session_key": "session:<uuid>"
    }
  }
}
```

`cancel` accepts only a direct child of the calling session. It aborts the
child's background chat run without deleting the session or its messages. The
response contains `sessionKey`, `aborted`, and `runId`.

## Prompt Profile

A normal session uses `prompt_profile = "chat"` and loads `SOUL.md`. A delegated
child uses `prompt_profile = "subagent"` and loads `SUBAGENT.md`. An empty
`SUBAGENT.md` is an error; `SOUL.md` is not substituted.

## Sandbox

A child stores its parent's resolved sandbox owner. Nested children keep the
same owner, so the session tree uses one sandbox container. A missing referenced
owner is an error. Terminal ownership checks remain session-specific inside the
shared container.

## Metrics

Delegated runs emit:

- `chelix_sub_agent_runs_total{mode,status}`;
- `chelix_sub_agent_run_duration_seconds{mode}`.
