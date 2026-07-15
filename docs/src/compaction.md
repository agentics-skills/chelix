# Compaction

Chelix has exactly two context-compaction entry points:

1. the agent loop reaches 85% of the session model's available input budget;
2. the user runs `/compact` (or calls the `chat.compact` RPC).

Both entry points use the **same model that is running the session** and append
a persistent **checkpoint** message. Automatic compaction summarizes the prefix
before a preserved triggering user/tool round; manual compaction summarizes the
full current context. No message or tool result is shortened, replaced, pruned,
or deleted.

## How it works

Automatic compaction is one transaction across the runner, ordered event
persistence, the append-only session store, and history reconstruction:

1. The runner builds the exact next provider request, including active tool
  schemas and any `BeforeLLMCall` payload changes.
2. Immediately before the provider call, it evaluates the context budget. Below
  85%, the ordinary request proceeds unchanged. At or above 85%, the loop
  pauses and that ordinary provider request is not sent.
3. The runner splits the already prepared `ChatMessage` vector with
  `split_off`. The older prefix becomes `summaryMessages`; the untouched tail
  becomes `continuationMessages`.
4. The runner returns both vectors, the exact active schemas, budget metadata,
  and accumulated run accounting to the chat layer.
5. The chat layer snapshots the ordered runner-event barrier and waits until
  every preceding event has been processed. The assistant tool-call frame and
  its tool results are therefore persisted before checkpoint creation.
6. The summary call uses the same provider, exact `summaryMessages`, and exact
  schemas. Chelix adds only one trailing user message containing the summary
  instructions. Existing messages are not rebuilt, trimmed, reordered, or
  normalized, so the shared provider prefix stays byte-identical for
  **prompt-cache reuse**.
7. The summary call alone receives the model's resolved `maxOutputTokens`. The
  model returns a structured summary (`<analysis>` + `<summary>` with eight
  sections); ordinary agent calls keep their existing output-limit behavior.
8. After receiving a non-empty summary, Chelix locates
  `continuationMessages` in physical persisted history. Tool-call IDs identify
  the preserved assistant frame; the associated user message is included when
  the continuation starts with that user turn.
9. Chelix appends a `checkpoint` containing the summary, model, provider,
  input/output usage, and the resulting absolute `messagesSummarized` history
  index. Earlier history is not modified.
10. Chelix rereads the session and verifies that the appended checkpoint is the
  latest checkpoint and reconstructs a non-empty active context. Only then is
  `auto_compact/done` emitted and the same run resumed, without repeating the
  original user message or adding a synthetic continuation prompt. Iteration,
  tool-call, usage, and raw-response accounting is carried across the resume.

### Continuation boundary

For a normal new turn, the boundary starts at the current user message. That
message and the first assistant tool-call/result round therefore stay together
outside the summary. Before each later tool round is appended, the boundary
moves to the start of that round. After multiple rounds, older completed rounds
may be summarized while the latest round remains verbatim so the model can see
its tool call and result when execution resumes.

The split operates on the provider-ready typed messages. Reconstructing or
normalizing either side would change the paused prompt, risk provider
prompt-cache corruption, and can detach a tool result from its assistant tool
call.

### Checkpoint reconstruction

`messagesSummarized` is an absolute index in physical persisted history, not
the length of `summaryMessages`. These values can differ because persisted
history has storage-only entries and runner events are stored asynchronously.

The latest checkpoint defines the active context in this order:

1. the checkpoint as a `<conversation-summary>` message;
2. the original persisted tail from `messagesSummarized` up to the checkpoint;
3. messages appended after the checkpoint.

Older checkpoints inside the retained tail are excluded. A `tool` or
`tool_result` message is replayed only after an assistant message containing
the matching tool-call ID; orphan results are excluded from provider context.

The first iteration after a checkpoint may bypass the 85% trigger only when
its prompt is strictly below `availableInputTokens`. This lets the model consume
the preserved round without an immediate compaction loop while retaining the
hard input limit. Later iterations use the normal 85% trigger. A preserved tail
at or above the hard limit triggers compaction again instead of being sent.

### Failure behavior

Compaction stops the run with an error if required model metadata is missing,
the summary request fails or returns empty text, the continuation cannot be
mapped to persisted history, the checkpoint append fails, or post-append
reread/reconstruction cannot verify the new checkpoint. A checkpoint is created
only after the summary and boundary are valid. If post-append verification
fails, the append-only checkpoint remains stored, but the current run emits an
error and does not resume or report compaction as done. Chelix never silently
resumes from partial, empty, superseded, or synthetic context.

### Invariants for future changes

All changes to this flow must preserve these properties:

- Keep the budget gate before the ordinary provider call.
- Estimate messages and schemas separately, and subtract schemas exactly once.
- Use resolved `maxInputTokens`; do not substitute `contextWindow` when it is
  missing.
- Preserve `summaryMessages`, `continuationMessages`, and active schemas exactly
  as prepared by the paused runner.
- Add only the trailing summary instruction to the summary request.
- Keep the current user and first tool round together; after later rounds,
  preserve the latest tool round outside the summary.
- Keep the ordered event barrier before persisted-boundary resolution.
- Derive `messagesSummarized` from physical history, not typed-vector length.
- Append checkpoints; never overwrite, prune, or normalize earlier history.
- Require post-append reread to verify the exact checkpoint as latest and
  reconstruct a non-empty context before reporting success or resuming.
- Reconstruct from the latest checkpoint and keep matching assistant
  tool-call/result pairs together.
- Resume the existing run; never duplicate the user message or add a synthetic
  resume prompt.
- Apply the summary output limit only to the summary request.
- Allow the post-checkpoint threshold bypass for one iteration only, and never
  at or above the hard available-input limit.

Because the history is append-only:

- **Forking works from any point.** Every message before the checkpoint is still
  in the session file, byte-identical.
- **The web UI shows the full conversation**, with a checkpoint card marking
  where each new context window begins.
- **Synchronous inter-session sends keep their natural final gate.** Automatic
  compaction does not inject an extra user instruction into the target run.
- **Iterative re-summarization builds on the previous checkpoint** — the
  summarization call itself sees the prior `<conversation-summary>` plus the
  tail, exactly like a regular turn would.

## Triggers

| Trigger                 | When                                                                            |
| ----------------------- | ------------------------------------------------------------------------------- |
| Agent-loop auto-compact | Prompt messages reach 85% of `maxInputTokens - active tool schema tokens`.      |
| Manual                  | `/compact` in the web UI or the `chat.compact` RPC.                             |

The 85% threshold is fixed and has no configuration switch or override. Manual
`/compact` summarizes the current context regardless of its size (unless the
session already ends with a checkpoint).

## Context-budget metadata

Every tool result records the exact budget calculation used before the LLM
iteration that produced its tool call. The tool card exposes it under **Context
budget**:

- `contextWindow`
- `maxInputTokens`
- `maxOutputTokens`
- `compactionRatio` (`85`)
- `promptTokens`
- `toolSchemaTokens`
- `availableInputTokens`
- `compactionBudget`
- `usagePercent`
- `compactionRequired`

The same metadata is included in the `auto_compact` lifecycle event when the 85%
trigger fires. These values come from the authoritative agent-loop check. The UI
derives its displayed threshold percentage as
`floor(promptTokens * 100 / compactionBudget)`, matching the backend trigger;
values above the threshold may exceed 100%.

## Channel notifications

When a session attached to a channel (Telegram, Discord, Matrix, WhatsApp, etc.)
is summarized, pending reply targets receive a short notice with the model,
token usage, and the number of messages checkpointed.

## Further reading

- `crates/agents/src/runner/helpers.rs` — exact prompt-budget calculation.
- `crates/agents/src/runner/non_streaming.rs` and
  `crates/agents/src/runner/streaming.rs` — pre-provider stop and continuation
  tracking.
- `crates/chat/src/run_with_tools.rs` — ordered persistence barrier, checkpoint
  transaction, and resume.
- `crates/chat/src/compaction.rs` — summarization prompt, cache-friendly request
  shape, persisted-boundary mapping, and checkpoint append logic.
- `crates/agents/src/model/convert.rs` — context construction: the latest
  checkpoint starts a fresh context window while preserving valid tool rounds.
- `crates/agents/src/runner/tests/compaction.rs` and the tests in
  `crates/chat/src/compaction.rs` and `crates/agents/src/model/convert.rs` —
  regression coverage for the invariants above.
- `references/vscode-copilot-chat` — the reference implementation
  (`summarizedConversationHistory.tsx`) the prompt is adapted from.
