# Compaction

Chelix has exactly two context-compaction entry points:

1. the agent loop reaches 85% of the session model's context window;
2. the user runs `/compact` (or calls the `chat.compact` RPC).

Both entry points use the **same model that is running the session** and append
a persistent **checkpoint** message. Automatic compaction summarizes the
prefix before a preserved triggering user/tool round; manual compaction
summarizes the full current context. No message or tool result is shortened,
replaced, pruned, or deleted.

## How it works

1. Immediately before every LLM request in the agent loop, Chelix estimates
  the exact request's messages and active tool schemas. At 85%, the loop
  pauses before sending that request.
2. Chelix chooses a continuation boundary like VS Code Copilot Chat: the
  current user message and first tool round are kept together, or only the
  latest tool round is kept after multiple rounds. The exact paused-request
  prefix before that boundary and the active tool schemas are sent to the
  same provider, with only the summarization instructions appended as one
  trailing user message. That prefix remains byte-identical and eligible for
  **prompt-cache reuse**.
3. The model produces a detailed structured summary (`<analysis>` +
   `<summary>` with eight sections: conversation overview, technical
   foundation, codebase status, problem resolution, progress tracking,
   active work state, recent operations, continuation plan).
4. The summary is appended to the session as a `checkpoint` message with
  metadata: model, provider, input/output tokens, and `messagesSummarized`,
  the absolute boundary between the summarized prefix and preserved tail.
5. The same agent run resumes immediately from the checkpoint without
  repeating the original user message or adding a synthetic continuation
  prompt. The new context is `<conversation-summary>` followed by the
  preserved triggering user/tool round and later messages verbatim.

Because the history is append-only:

- **Forking works from any point.** Every message before the checkpoint is
  still in the session file, byte-identical.
- **The web UI shows the full conversation**, with a checkpoint card marking
  where each new context window begins.
- **Synchronous inter-session sends keep their natural final gate.** Automatic
  compaction does not inject an extra user instruction into the target run.
- **Iterative re-summarization builds on the previous checkpoint** — the
  summarization call itself sees the prior `<conversation-summary>` plus
  the tail, exactly like a regular turn would.

## Triggers

| Trigger | When |
|---|---|
| Agent-loop auto-compact | The exact next LLM request reaches 85% of the provider context window. |
| Manual | `/compact` in the web UI or the `chat.compact` RPC. |

The 85% threshold is fixed and has no configuration switch or override.
Manual `/compact` summarizes the current context regardless of its size
(unless the session already ends with a checkpoint).

## Context-budget metadata

Every tool result records the exact budget calculation used before the LLM
iteration that produced its tool call. The tool card exposes it under
**Context budget**:

- `contextWindow`
- `compactionRatio` (`85`)
- `currentTokens`
- `compactionBudget`
- `usagePercent`
- `compactionRequired`

The same metadata is included in the `auto_compact` lifecycle event when the
85% trigger fires. These values come from the authoritative agent-loop check;
the UI does not recalculate them.

## Channel notifications

When a session attached to a channel (Telegram, Discord, Matrix, WhatsApp,
etc.) is summarized, pending reply targets receive a short notice with the
model, token usage, and the number of messages checkpointed.

## Further reading

- `crates/agents/src/runner/helpers.rs` — exact prompt-budget calculation.
- `crates/chat/src/compaction.rs` — summarization prompt, cache-friendly
  request shape, and checkpoint append logic.
- `crates/agents/src/model/convert.rs` — context construction: the latest
  checkpoint starts a fresh context window.
- `references/vscode-copilot-chat` — the reference implementation
  (`summarizedConversationHistory.tsx`) the prompt is adapted from.
