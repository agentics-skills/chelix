# Session Message Deletion

User messages in agent sessions can be deleted from the web UI. Deleting a user
message removes that message and the complete tail after it, including assistant
messages, tool-call assistant messages, tool results, and any other persisted
session-history messages after the selected user turn.

The UI does not ask for confirmation. The delete action is intentionally compact
and is placed under the user-message copy button.

The backend operation is exposed as the `sessions.truncate_tail` RPC. It accepts
`key` and `target: { messageId, generation }`, identifying a committed user
snapshot. The server resolves its canonical boundary under the session mutation
reservation. Missing sessions, removed targets, stale generations, uncommitted
records, and non-user targets are rejected
(`crates/gateway/src/session/maintenance.rs`,
`crates/sessions/src/ui_history_engine.rs`).

Pending browser sends receive their delete action only after the engine confirms
their `clientMessageId` and canonical binding.

Before truncating a session tail, the gateway cancels queued messages and aborts
the active chat run for that session. A shared per-session mutation coordinator
blocks new chat turns while the truncation is reserved, waits for any active
turn to release the session after abort, validates the canonical cut, and then
truncates both the JSONL journal and semantic SQLite snapshots.

After truncation, session metadata is updated: message counts are reduced, the
active run state is cleared, and the preview is replaced with the
retained-history preview or cleared if no preview remains. The engine rotates
the session generation and subscriptions receive an authoritative baseline.
Browser pages and action targets are checked against that generation.

## Media pruning and forks

Truncation prunes media files in the current session media directory when they
are no longer referenced by the retained current-session history.

Current fork behavior is not yet containerized. A fork can still reference media
stored in the parent session media directory. If a parent-session delete prunes
media that only a fork still references, that fork media link can become broken.

Future fork work should use a full snapshot/container wrapper with independent
copied data for forked sessions. That design must avoid rewriting
prompt-cache-sensitive media links in a way that degrades provider prompt
caching.
