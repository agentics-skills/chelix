# Prompt Queue

A session runs one agent turn at a time. Prompts submitted while a run owns the
session enter a durable per-session **prompt queue** instead of starting a
competing run.

## Behavior

1. `chat.send` tries to acquire the session turn permit.
2. When the permit is free, the prompt starts a run as usual.
3. When a run already owns the session, the prompt is appended to the session
   queue and `chat.send` returns `{ "queued": true, "prompts": [...] }` with the
   full queue.
4. When the owning run reaches its final gate and releases the permit, the whole
   queue is claimed and replayed as **one** agent run.

Because the queue is replayed once, several prompts sent during one run produce
a single assistant turn instead of one turn per prompt.

The replay is pinned to the session that owns the queue. A queued prompt is
session state, so it runs in its own session even when the submitting client
switched sessions or disconnected while the previous run was still going.

## Claiming and restoring

Claiming empties the queue in one statement, so a prompt is either still pending
and cancellable, or already committed to a replay — never both.

A claim is only consumed once the prompts reach session history. `chat.send` can
also succeed without persisting anything: a `MessageReceived` hook may reject the
message, and a session that became busy again defers the replay. The replay
result therefore reports how many claimed prompts were persisted, and anything
else returns the whole batch to the queue, ahead of prompts queued in the
meantime. Nothing the user typed is dropped silently.

## Batch shape

The claimed queue enters the replay run as consecutive `user` messages in
submission order, immediately before the request that carries them. Message
content is never concatenated or rewritten: each queued prompt keeps its own
text, images, and attached documents.

The last queued prompt supplies the request parameters (model and reasoning
effort), so the run uses the selection the user made most recently.

The whole batch is persisted as one atomic append before the run starts, so
stored history, the provider request, and the UI show the same order, and a
rejected write leaves history exactly as it was.

Replayed `user_message` events carry `replayed: true`. The submitting client
dropped its optimistic bubble when the prompt was queued, so it renders these
messages instead of suppressing them as its own echo.

## Persistence and synchronization

The queue lives in the `session_prompt_queue` table (`chelix.db`), created by
`crates/sessions/migrations/20260815090000_session_prompt_queue.sql`.

- It survives page reloads, reconnects, and gateway restarts.
- Every mutation broadcasts a `chat` event with `state: "prompt_queue"` carrying
  the full queue snapshot, so all connected clients render the same prompts.
- `sessions.switch` returns the current queue in `queuedPrompts`, so a client
  that just connected renders it without waiting for an event.

## Compaction

Queueing does not change context compaction. Queued prompts are held outside the
agent loop and enter a run only after the previous run released the session, so
they never modify a paused prompt or the shared provider prefix. See
[Compaction](compaction.md).

## Cancelling

Clearing the queue is explicit:

- `chat.prompt_queue.cancel` with `promptId` removes one prompt;
- `chat.prompt_queue.cancel` without `promptId` removes the whole session queue;
- `chat.clear` and `sessions.truncate_tail` clear the queue of the affected
  session;
- `sessions.delete` drops the queue together with the session, so a reused
  session key never inherits stale prompts.

Cancelling an unknown `promptId` fails instead of reporting success, so a stale
client cannot believe it removed something that is still queued. A prompt that
was already claimed for a replay is no longer in the queue and reports the same
failure.

## API

| Surface | Operation                                                    |
| ------- | ------------------------------------------------------------ |
| RPC     | `chat.prompt_queue.list`, `chat.prompt_queue.cancel`         |
| GraphQL | `chat.queuedPrompts` query, `chat.cancelQueuedPrompts`       |
| Channel | `/queue <message>` queues a prompt for the next turn         |

## Further reading

- `crates/sessions/src/prompt_queue.rs` — durable storage.
- `crates/chat/src/prompt_queue.rs` — queueing, snapshots, and batch merging.
- `crates/chat/src/service/chat_impl/send.rs` — final gate and replay.
