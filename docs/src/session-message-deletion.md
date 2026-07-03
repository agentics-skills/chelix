# Session Message Deletion

User messages in agent sessions can be deleted from the web UI. Deleting a user message removes that message and the complete tail after it, including assistant messages, tool-call assistant messages, tool results, and any other persisted session-history messages after the selected user turn.

The UI does not ask for confirmation. The delete action is intentionally compact and is placed under the user-message copy button.

The backend operation is exposed as the `sessions.truncate_tail` RPC. It accepts a `key` plus either `messageIndex`/`historyIndex` or `seq` identifying the target user message. The operation rejects missing sessions, missing targets, out-of-range indices, and non-user targets.

Before truncating a session tail, the gateway cancels queued messages and aborts the active chat run for that session. A shared per-session mutation coordinator blocks new chat turns while the truncation is reserved, waits for any active turn to release the session after abort, and then rewrites the JSONL history.

After truncation, session metadata is updated: message counts are reduced, the active run state is cleared, and the preview is replaced with the retained-history preview or cleared if no preview remains.

## Media pruning and forks

Truncation prunes media files in the current session media directory when they are no longer referenced by the retained current-session history.

Current fork behavior is not yet containerized. A fork can still reference media stored in the parent session media directory. If a parent-session delete prunes media that only a fork still references, that fork media link can become broken.

Future fork work should use a full snapshot/container wrapper with independent copied data for forked sessions. That design must avoid rewriting prompt-cache-sensitive media links in a way that degrades provider prompt caching.
