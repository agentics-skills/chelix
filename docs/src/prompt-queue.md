# queuedPrompts

A session executes one agent turn at a time. When `chat.send` receives another
user prompt during an active turn, the backend stores that prompt in the internal
`queuedPrompts` service.

The service stores only the canonical session key and the prompt content. It does
not store or resolve a provider, model, reasoning effort, tools, connection, run,
or other execution state.

## Queue records

`QueuedPromptContent` is the closed representation of one accepted user prompt.
It contains the unchanged text or ordered multimodal content together with its
documents, media references, client sequence, input and reply medium, and current
channel metadata.

Each `QueuedPrompt` contains exactly:

- an auto-incremented numeric `id`;
- the canonical session key;
- `QueuedPromptContent`.

`QueuedPromptsStatus` contains the canonical session key and the complete current
list for that session in ascending ID order. The dock preview is calculated from
`QueuedPromptContent`; it is not stored in the queue.

## Storage and ordering

The `session_prompt_queue` table has three columns:

```sql
id          INTEGER PRIMARY KEY AUTOINCREMENT
session_key TEXT    NOT NULL
content     TEXT    NOT NULL
```

One index supports `WHERE session_key = ? ORDER BY id ASC`. The numeric ID is the
FIFO order; there is no separate position.

All service operations pass through one Tokio `mpsc` receiver. That receiver
executes typed commands sequentially in arrival order. Callers cannot access the
SQL store directly, and an unavailable command loop is an explicit error.

## Service operations

- `enqueue(session_id, content)` inserts one row, reads the complete status of
  that session in ascending ID order, and returns it.
- `remove(id)` finds the owning session and deletes exactly that row in one
  transaction. It returns the resulting status, and an unknown ID is an error.
- `drain(session_id)` reads the ordered session batch and deletes it in one
  transaction. It returns both the removed prompts and the empty resulting
  status. An empty queue returns an empty batch and empty status.
- `clear(session_id)` deletes that session's rows and returns the SQL result. It
  is used when the session itself is deleted.
- `status(session_id)` returns the complete current status without changing the
  queue.

## API synchronization

The queue RPC methods are:

- `chat.queued_prompts.status` with `sessionKey`;
- `chat.queued_prompts.remove` with only the numeric prompt `id`.

A successful enqueue, removal, or drain produces one canonical status. The RPC
response and the `chat` WebSocket event use that same status without another
read or local adjustment. The event has `state: "prompt_queue"` and carries the
status in `status`.

`sessions.switch` reads the backend status and returns it in `queuedPrompts`.
Failure to read the status fails the session load.

## Agent-turn integration

After every complete final gate, the owning turn calls `drain(session_id)` once.
The resulting status is broadcast immediately. An empty batch ends processing.

A non-empty batch enters the existing session in one call as the canonical
session key and ordered `QueuedPromptContent` values. The session boundary uses
its persisted execution settings. Every prompt remains a separate user message,
and the contents are neither concatenated nor rewritten. The private batch
boundary does not call public `chat.send` or dispatch the inbound
`MessageReceived` hook.

Leading values are preceding user messages in that single turn, while the tail
value is its current user message. The message-level reply target, reply medium,
and sender identity belong to that tail value; they do not fall back to a leading
value.

The batch starts one complete agent turn. That turn has its own complete final
gate and performs its own single drain afterward. An execution error is reported
through the existing error paths, and the already removed batch is not inserted
into the queue again.

## UI behavior

The existing queued-message dock is rendered only from a complete backend
status. WebSocket events, `sessions.switch`, and RPC responses all use the same
full-replacement function.

The UI keeps no queue map, cache, local-storage value, or optimistic queue state.
A status for an inactive session is ignored; switching to that session obtains a
fresh status from `sessions.switch`. Removing an item sends only its numeric ID
and waits for the returned backend status before replacing the dock.
