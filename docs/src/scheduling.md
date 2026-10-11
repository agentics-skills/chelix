# Scheduling (Cron Jobs)

Chelix includes a built-in cron system that lets the agent schedule and manage
recurring tasks. Jobs can fire agent turns, send system events, or trigger other
actions on a flexible schedule.

## Schedule Types

Jobs support several schedule kinds:

| Kind    | Fields                | Description                                     |
| ------- | --------------------- | ----------------------------------------------- |
| `every` | `every_ms`            | Repeat at a fixed interval (milliseconds)       |
| `cron`  | `expr`, optional `tz` | Standard cron expression (e.g. `"0 */6 * * *"`) |
| `at`    | `at_ms`               | Run once at a specific Unix timestamp (ms)      |

## Cron Tool

The agent manages jobs through the built-in `cron` tool. Available actions:

- **`add`** — Create a new job
- **`list`** — List all jobs
- **`run`** — Trigger a job immediately
- **`update`** — Patch an existing job (name, schedule, enabled, etc.)
- **`remove`** — Delete a job
- **`runs`** — View recent execution history for a job

An `agentTurn` can optionally override model selection with one complete
`modelOverride` pair:

```json
{
  "kind": "agentTurn",
  "message": "Check deploy status",
  "modelOverride": {
    "model": "openai::gpt-5.2",
    "reasoningEffort": "medium"
  }
}
```

Omit `modelOverride` to use the persisted session pair. If it is present, both
fields are required; the model must exactly match `models.list`, and the effort
must be non-empty and supported by that model.

### One-Shot Jobs

Set `deleteAfterRun: true` to automatically remove a job after its first
execution. Combined with the `at` schedule, this is useful for deferred one-time
tasks (reminders, follow-ups).

## Channel Delivery

Background agent turns can deliver their final output to a configured channel
account/chat after the run completes.

Use all of the following together:

- `sessionTarget: "isolated"`
- `payload.kind: "agentTurn"`
- `payload.deliver: true`
- `payload.channel: "<account_id>"`
- `payload.to: "<chat_or_peer_id>"`

Example:

```json
{
  "action": "add",
  "job": {
    "name": "daily summary",
    "schedule": { "kind": "cron", "expr": "0 9 * * *", "tz": "Europe/Paris" },
    "sessionTarget": "isolated",
    "payload": {
      "kind": "agentTurn",
      "message": "Summarize yesterday's activity and send it to Telegram.",
      "deliver": true,
      "channel": "my-telegram-bot",
      "to": "123456789"
    }
  }
}
```

Channel delivery is separate from session targeting. The cron job still runs in
an isolated cron session, then Chelix forwards the finished output to the
requested channel destination.

## Session Targeting

Each job specifies where its agent turn runs:

| Target            | Description                                                                                                            |
| ----------------- | ---------------------------------------------------------------------------------------------------------------------- |
| `"main"`          | Inject a `systemEvent` into the main session. Use this for reminders that should continue the main conversation.       |
| `"isolated"`      | Run an `agentTurn` in a throwaway cron session. Use this for background jobs and channel delivery.                     |

## Security

See [Cron Job Security](security.md#cron-job-security) for rate limiting,
sandbox isolation, and job notification details.

## Metrics

| Metric                                   | Type      | Description                  |
| ---------------------------------------- | --------- | ---------------------------- |
| `chelix_cron_jobs_scheduled`             | Gauge     | Number of scheduled jobs     |
| `chelix_cron_executions_total`           | Counter   | Job executions               |
| `chelix_cron_execution_duration_seconds` | Histogram | Job duration                 |
| `chelix_cron_errors_total`               | Counter   | Failed jobs                  |
| `chelix_cron_stuck_jobs_cleared_total`   | Counter   | Jobs exceeding 2h timeout    |
| `chelix_cron_input_tokens_total`         | Counter   | Input tokens from cron runs  |
| `chelix_cron_output_tokens_total`        | Counter   | Output tokens from cron runs |
