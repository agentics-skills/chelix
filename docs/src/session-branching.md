# Session Branching

Session branching (forking) lets you create an independent copy of a
conversation at any point. The new session diverges without affecting the
original — useful for exploring alternative approaches, running "what if"
scenarios, or preserving a checkpoint before a risky prompt.

## /fork Command

The quickest way to fork — type in the chat input or on any channel:

```
/fork                    # fork with auto-generated label
/fork experiment-a       # fork with a custom label
```

Available in the web UI, Telegram, Matrix, and all other
channels. See [Slash Commands](commands.md) for the full list.

## Forking from the UI

There are three ways to fork a session:

- **`/fork` command** — type `/fork [label]` in the chat input.
- **Chat header** — click the **Fork** button in the header bar (next to
  Delete). This is visible for every session except cron sessions.
- **Message action** — fork through the selected assistant message using its
  stable message ID and session generation.

Header and `/fork` actions select the maximal confirmed history prefix with a
complete canonical boundary. If active content or an interleaved provider segment
shortens that prefix, the response reports the adjustment and the UI displays a
notification. Message actions use the exact selected boundary and reject an
unfinished or interleaved prefix.

Forked sessions appear **indented** under their parent in the sidebar, with a
branch icon to distinguish them from top-level sessions. The metadata line shows
`fork@N` where N is the exclusive UI history position at which the fork occurred.

## Agent Tool

The agent can also fork programmatically using the `branch_session` tool:

```json
{
  "fork_point": 5,
  "label": "explore-alternative"
}
```

- **`label`** — label for the new session (required).
- **`fork_point`** — an exclusive UI history position (0-based). Snapshots before
  this position and their canonical prefix are copied. An explicit boundary must
  be confirmed and must not split an interleaved canonical segment.
- When `fork_point` is omitted, the maximal confirmed prefix is selected. A
  nonempty source with no confirmed prefix is rejected. An explicit zero boundary
  and an empty source are valid.

The tool returns `sessionKey`, `forkPoint`, `sourceEnd`, `boundaryAdjusted`,
`boundaryReasons`, and session metadata. `sourceEnd` is the source snapshot's
exclusive end position. Adjusted defaults report the applied reasons
`active_content` and/or `interleaved_segment`. Explicit boundaries report
`boundaryAdjusted: false` and `boundaryReasons: []`.

## RPC Method

The `sessions.fork` RPC method is the underlying mechanism:

```json
{ "key": "main", "forkPoint": 5, "label": "my-fork" }
```

The RPC accepts either `forkPoint` or `target` containing `messageId` and
`generation`. A target includes the addressed snapshot in the copied prefix.
Both explicit forms reject a boundary that cannot be copied exactly.

Success returns the same boundary fields as `branch_session`, including
`sourceEnd`, `boundaryAdjusted`, and `boundaryReasons` for every request form.
The selected position is checked against the metadata field's integer range
before the destination is written.

## What Gets Inherited

When forking, the new session inherits:

| Inherited                   | Not inherited    |
| --------------------------- | ---------------- |
| Messages (up to fork point) | Worktree branch  |
| Model selection             | Sandbox settings |
| Project assignment          | Channel binding  |
| Agent ID                    |                  |
| MCP disabled flag           |                  |
| Node assignment             |                  |

## Parent-Child Relationships

Fork relationships are stored directly on the `sessions` table:

- **`parent_session_key`** — the key of the session this was forked from.
- **`fork_point`** — the exclusive UI history position where the fork occurred.

These fields drive the tree rendering in the sidebar. Sessions with a parent
appear indented under it; deeply nested forks indent further.

```admonish warning title="Deleting a parent"
Deleting a parent session cascades to all of its fork descendants. The delete
operation removes each descendant before the parent, and also clears active
channel mappings that pointed at any deleted session.
```

Session deletion also removes session-specific memory export Markdown files and
their indexed chunks/embedding blobs from memory search. The global embedding
cache is content-hash based and is not treated as session-owned data.

## Navigation After Delete

When you delete a forked session, the UI navigates back to its parent session.
If the deleted session had no parent, the parent no longer exists, or the parent
was part of the same cascaded deletion, it falls back to the next sibling or
`main`.

## Archive in the UI

The web UI also lets you archive sessions when you want to keep them without
leaving them in the main sidebar list.

- Open **More controls** for a session and click **Archive**.
- Archived sessions are hidden from the default sidebar list.
- Enable **Show archived sessions** in the sidebar to reveal and restore them.

Archive is available for any non-`main` session, including cron and
channel-bound chats, except when the session is the current active session for
its bound channel chat. That prevents hiding the live Telegram, or
similar chat out from under the channel router.

```admonish info title="Independence"
A forked session is fully independent after creation. Changes to the parent
do not propagate to the fork, and vice versa.
```
