# Sub-Agent Delegation

`sub_agent` delegates work to persisted direct-child sessions. It has eight
actions: `explore`, `run`, `status`, `list`, `result`, `send`, `attach`, and
`cancel`.

The root object requires `action`. `action` must contain exactly one action name
and its parameter object. Unknown fields are rejected. No parameter has a
default value.

The public identifier for a child is only `sessionKey`. `status` is the current
session state. A new prompt makes the session `running`; the next final gate
becomes the current result.

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

An agent is available only when
`<data_dir>/agents/<id>/SUBAGENT.md` is non-empty after trimming. Every
configured agent already has a required model/reasoning pair validated through
the live model registry.

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
first user message is the exact `task` value, prefixed with
`[From the "<name>" agent]` followed by a blank line when the sender agent has
`prepend_sender_badge = true`. Normal tool-policy layers control
its access to the shared tool registry.

Both modes start the child the same way. A queued or rejected start is an
error. Stopping the parent session does not stop the child.

A blocking response waits for the child's next final gate and contains:

- `sessionKey`;
- `agentId`;
- `mode`;
- `status`: `completed` or `cancelled`;
- `text` when `status` is `completed`.

A background response returns immediately and contains:

- `sessionKey`;
- `agentId`;
- `mode`;
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
`updatedAt`. `status` is `running`, `cancelled`, `completed`, or `idle`.

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

`result` accepts a direct child of the calling session in any mode. It reads
the current session state:

- `running` while the child is executing;
- `cancelled` after a user stop or `cancel`;
- `completed` with the last final-gate assistant `text`;
- `idle` when the child has no current final gate.

A new prompt makes the previous final gate no longer current because the
session is `running` again.

## Send

```json
{
  "action": {
    "send": {
      "session_key": "session:<uuid>",
      "mode": "blocking",
      "message": "Continue with the next step."
    }
  }
}
```

`send` accepts a direct child of the calling session. It uses the same session
status as `list`. `running` is an error:
`sub-agent session "..." is still running; request result for the current task and wait for it to finish`.

`cancelled`, `completed`, and `idle` send `message` as the next user prompt.
`mode` is `blocking` or `background` and matches `run`. The user message
is the exact `message` value, prefixed with `[From the "<name>" agent]`
followed by a blank line when the sender agent has `prepend_sender_badge = true`.

A blocking response waits for the child's next final gate and contains:

- `sessionKey`;
- `agentId`;
- `mode`;
- `status`: `completed` or `cancelled`;
- `text` when `status` is `completed`.

A background response returns immediately and contains:

- `sessionKey`;
- `agentId`;
- `mode`;
- `status: "running"`.

## Attach

```json
{
  "action": {
    "attach": {
      "session_key": "session:<uuid>"
    }
  }
}
```

`attach` accepts a direct child of the calling session. It joins the current
execution of that session. If the child is already finished, it returns the
last final gate immediately. If the child is executing, it waits for the next
final gate and returns that result. The response shape matches `result`.

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

`cancel` accepts only a direct child of the calling session. It stops the
child's current execution without deleting the session or its messages. The
response contains `sessionKey` and `aborted`. UI Stop on that session does the
same thing.

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

Delegated runs emit `chelix_sub_agent_runs_total{mode,status}` when a run
starts. Blocking waits also emit that counter for the terminal status and
`chelix_sub_agent_run_duration_seconds{mode}` for the wait.
