# Compaction

When a chat session approaches the model's context window (or you run
`/compact`), Chelix summarizes the conversation with the **same model that
is running the session** and appends a persistent **checkpoint** message to
the session history. Nothing in the existing history is modified or
deleted — the next context window simply starts from the checkpoint.

## How it works

1. The exact stored session history (unmodified) is sent to the session's
   provider together with a comprehensive summarization prompt adapted from
   the VS Code Copilot Chat reference implementation.
2. The model produces a detailed structured summary (`<analysis>` +
   `<summary>` with eight sections: conversation overview, technical
   foundation, codebase status, problem resolution, progress tracking,
   active work state, recent operations, continuation plan).
3. The summary is appended to the session as a `checkpoint` message with
   metadata: model, provider, input/output tokens, and the number of
   messages it covers.
4. From the next turn on, the LLM context starts at the latest checkpoint:
   the summary is injected as a `<conversation-summary>` user message and
   only messages after the checkpoint are included verbatim.

Because the history is append-only:

- **Forking works from any point.** Every message before the checkpoint is
  still in the session file, byte-identical.
- **The web UI shows the full conversation**, with a checkpoint card marking
  where each new context window begins.
- **Iterative re-summarization builds on the previous checkpoint** — the
  summarization call itself sees the prior `<conversation-summary>` plus
  the tail, exactly like a regular turn would.

## Triggers

| Trigger | When |
|---|---|
| Pre-emptive auto-compact | The estimated next request exceeds 85 % of the token budget (see below). |
| Context-overflow retry | The provider rejects a request with a context-window error; Chelix summarizes and retries once. |
| Manual | `/compact` in the web UI or the `chat.compact` RPC. |

## Configuration

All compaction settings live under `[chat.compaction]` in `chelix.toml`:

```toml
[chat.compaction]
enabled = true          # Automatic summarization on context pressure.
threshold_tokens = 0    # Token budget override. 0 = use the model's context window.
```

- `enabled` — when `false`, automatic summarization never fires
  (pre-emptive and overflow-retry paths are skipped). Manual `/compact`
  keeps working.
- `threshold_tokens` — overrides the token budget used for the
  pre-emptive trigger. With the default `0`, the budget is the session
  model's context window. The trigger fires at `budget × 0.85`, matching
  the reference implementation's summarization safety factor: on a 200 K
  model, summarization starts at 170 K estimated tokens.

Manual `/compact` ignores the trigger math and summarizes whatever is in
the session (unless it already ends with a checkpoint).

## Channel notifications

When a session attached to a channel (Telegram, Discord, Matrix, WhatsApp,
etc.) is summarized, pending reply targets receive a short notice with the
model, token usage, and the number of messages checkpointed.

## Further reading

- `crates/chat/src/compaction.rs` — summarization prompt, threshold math,
  and checkpoint append logic.
- `crates/agents/src/model/convert.rs` — context construction: the latest
  checkpoint starts a fresh context window.
- `references/vscode-copilot-chat` — the reference implementation
  (`summarizedConversationHistory.tsx`) the prompt is adapted from.
