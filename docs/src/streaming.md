# Streaming Architecture

This document explains how streaming responses work in Chelix, from the LLM
provider through to the web UI.

## Overview

Chelix supports real-time token streaming for LLM responses, providing a much
better user experience than waiting for the complete response. Streaming works
even when tools are enabled, ensuring every provider output item maintains a
canonical identity and output position without loss through live streaming,
persistence, reload, and subsequent provider request replay.

## Components

### 1. Canonical Provider Output Model (`crates/common/src/provider_output.rs`)

Chelix defines concrete typed structures for provider responses:

- `ProviderSegmentId`: immutable identity for a provider response/attempt.
- `ProviderSegmentOutcome`: active, completed, incomplete, failed, cancelled, or transport_error.
- `ProviderItemId`: provider-issued or canonical ingress identity.
- `ProviderItemPosition`: 0-based position in the provider's output array.
- `ProviderItemUpdate`: append-only stream update addressed to a specific item.
- `ProviderSegmentMaterializer`: single Rust materializer verifying monotonic sequences and constructing ordered segments.

### 2. StreamEvent Enum (`crates/agents/src/model.rs`)

The `StreamEvent` enum defines all events that can occur during a streaming LLM
response:

```rust
pub enum StreamEvent {
    /// Append-only provider item update carrying canonical segment ID, item ID, position, seq, and payload.
    ProviderItemUpdate(chelix_common::ProviderItemUpdate),

    /// Provider response/attempt segment opened.
    SegmentStart {
        segment_id: chelix_common::ProviderSegmentId,
    },

    /// Provider response/attempt segment closed.
    SegmentClose {
        segment_id: chelix_common::ProviderSegmentId,
        outcome: chelix_common::ProviderSegmentOutcome,
        usage: Option<Usage>,
    },

    /// Text content delta.
    Delta(String),

    /// Raw provider event payload (for debugging API responses).
    ProviderRaw(serde_json::Value),

    /// A tool call has started (content_block_start with tool_use).
    ToolCallStart { id: String, name: String, index: usize },

    /// Streaming delta for tool call arguments (JSON fragment).
    ToolCallArgumentsDelta { index: usize, delta: String },

    /// A tool call's arguments are complete.
    ToolCallComplete { index: usize },

    /// Stream completed successfully.
    Done(Usage),

    /// An error occurred.
    Error(String),
}
```

### 3. LlmProvider Trait (`crates/agents/src/model.rs`)

The `LlmProvider` trait defines streaming methods accepting `Vec<ChatMessage>`:

- `stream()` — Text streaming.
- `stream_with_tools()` — Streaming with tool schemas passed to the API.
- `stream_with_tools_and_options()` — Streaming with tool schemas and
  `CompletionOptions`, carrying `tool_choice` and a per-request `max_output_tokens`.

The default implementations reject unsupported tools, forced tool selection,
and output limits with `StreamEvent::Error`. OpenAI serializes the output limit
as `max_completion_tokens` in Chat Completions SSE and `max_output_tokens` in
Responses SSE and WebSocket requests. The configured transport also applies to
session titles, memory-forget planning, and compaction.

`collect_stream()` collects a successful stream into `CompletionResponse` for
session titles, memory-forget planning, and compaction. It retains text, tool
calls, terminal usage, canonical provider segments, and bounded raw events.
Canonical segments retain received item identities and positions at the
collection boundary. Provider errors, unsuccessful segment outcomes, and EOF before `Done` are
returned as errors. Silent memory turns use the streaming agent runner with
callbacks omitted; tool execution and canonical item replay use that runner.

The trait also exposes `supports_tools()`, `reasoning_effort()`, and
`with_reasoning_effort()` for provider capability discovery.

### 4. Agent Runner (`crates/agents/src/runner/streaming.rs`)

The `run_agent_loop_streaming()` function orchestrates the streaming agent loop:

```
┌──────────────────────────────────────────────────────────────────┐
│                         Agent Loop                               │
│                                                                  │
│  1. Call provider.stream_with_tools()                            │
│                                                                  │
│  2. While the provider stream has events:                        │
│     ├─ SegmentStart → initialize ProviderSegmentMaterializer     │
│     ├─ ProviderItemUpdate → apply to materializer and emit       │
│     ├─ Delta(text) → emit RunnerEvent::TextDelta                 │
│     ├─ ToolCallStart → emit Created and accumulate the call      │
│     ├─ ToolCallArgumentsDelta → emit InputStreaming              │
│     ├─ ToolCallComplete → mark arguments complete                │
│     ├─ SegmentClose → close materializer with terminal outcome   │
│     ├─ Done → record usage                                       │
│     └─ Error → emit Cancelled for started calls, then retry/fail  │
│                                                                  │
│  3. Finalize arguments and emit InputReady                       │
│     └─ The canonical assistant tool-call frame is published here │
│                                                                  │
│  4. Execute calls concurrently through ToolInvocationExecutor    │
│     ├─ Validate → Rejected on pre-dispatch refusal               │
│     ├─ Emit WaitingForExecution                                  │
│     ├─ Run BeforeToolCall, then emit Executing                   │
│     ├─ Emit backend ExecutionProgress while useful work runs     │
│     └─ Emit ResultReady, then Completed                          │
│                                                                  │
│  5. Append terminal tool outputs to provider messages            │
│                                                                  │
│  6. Loop back to step 1                                          │
└──────────────────────────────────────────────────────────────────┘
```

### 5. Chat Service (`crates/chat/src/run_with_tools.rs`)

`ui_history_ingress.rs` copies provider updates, segment boundaries, provider
errors, and tool lifecycle stages into `UiHistoryRun`. The ordered callbacks in
`agent_loop.rs` perform this copy before forwarding records to `StreamJournal`.
Input streaming returns after enqueue; other lifecycle stages await their
canonical processor receipt. The stream-only and external-agent paths also copy
provider items before journal persistence.

`UiHistoryEngine` in `crates/sessions/src/ui_history_engine.rs` owns the semantic
conversation. Each provider segment and tool invocation has one stable message
ID, position, and revision. Session generation identifies the current history
lineage. User, notice, system, checkpoint, and final assistant records enter the
engine at the common `SessionStore` append boundary.

| Input | Semantic history |
| --- | --- |
| Provider item update | Updates the owning assistant snapshot and publishes a revision. |
| Segment close | Retains the segment outcome on the same snapshot. |
| Tool lifecycle stage | Replaces the invocation snapshot while retaining accumulated input. |
| Provider error | Creates an error entity with raw error, received details, run/segment identity and retry delay. |
| Auto-continue or loop intervention | Appends a notice through the common history ingress. |
| Final assistant record | Confirms the canonical binding of the copied assistant snapshot. |

SQLite stores accumulated semantic snapshots in `ui-history.sqlite`; the
coalescing writer waits 50 ms between dirty notifications and flush work.
JSONL stores the provider-context journal. Canonical bindings are confirmed
only after a successful append. Terminal run success waits for receipts and
snapshot flush. Persistence failure publishes a failed session revision and
refuses further run ingress.

Raw API debug payloads (`llmApiResponse`) remain in the canonical assistant record.
The engine projects assistant text, reasoning, tool calls, ordered provider items
and metadata into its in-memory snapshot. `UiSnapshot.content` uses a typed
assistant serialization view for SQLite, history tools, HTTP and WebSocket
responses; canonical record serialization and addressed journal updates retain the
raw debug payload.

Tool invocation updates use the shared `ToolLifecycleEvent` contract:

| Stage | Meaning |
| --- | --- |
| `created` | The provider announced the invocation; the UI can create its bubble immediately. |
| `input_streaming` | One JSON argument fragment is emitted; accumulated argument text lives in the active invocation and UI snapshots rather than the event. |
| `input_ready` | Arguments decoded successfully; the canonical assistant tool-call frame is persisted before execution. |
| `waiting_for_execution` | Pre-dispatch validation passed and the shared executor reached the execution boundary. |
| `executing` | The implementation is about to run with the effective public arguments. |
| `execution_progress` | Backend-authored elapsed time and progress text while the implementation future is pending. |
| `result_ready` | The agent-facing result has been prepared, before terminal completion. |
| `completed` | Terminal success or execution failure with result/error fields. |
| `rejected` | Terminal pre-execution refusal with the original arguments and reason. |
| `cancelled` | Terminal cancellation with an optional argument snapshot and reason. |

Persisted lifecycle records use `role: "tool_lifecycle"` and retain `runId`,
per-call `sequence`, `emittedAtMs`, and received `contextBudget`. The semantic
snapshot carries `accumulatedArguments`, the owning `assistantId`, and
presentation metadata. Terminal stages remain in semantic history.

`AgentTool::ui_presentation` can provide a text, Markdown, or diff document and
metadata at each lifecycle stage. `RunnerToolLifecycleEvent` carries this
UI-only representation to the engine. `UiHistorySession::update_presentation`
addresses stored tool, checkpoint, and error presentations by ID and generation.

The runner injects the exact provider tool-call ID into the hidden
`_tool_call_id` execution context before implementation validation and restores
trusted context after a `BeforeToolCall` hook rewrites arguments. Internal
underscore-prefixed fields are removed from caller-visible lifecycle arguments.
The waiting [A2UI](a2ui.md) tool uses this ID with the trusted session and run
IDs to route a browser action to one active invocation.

### 6. Segments, Retries, and Replay

A segment is one provider response attempt. A retry or a tool boundary closes
the current segment and opens the next one; it never deletes, overwrites, or
merges an adjacent segment.

**Retry keeps what the failed attempt produced.** When a stream fails and the
runner decides to retry, it closes the segment as `transport_error` and appends
the items that attempt already produced to the messages the next attempt sees
(`crates/agents/src/runner/streaming.rs`). The next attempt therefore starts
from what the model has already said rather than from nothing.

**Unclosed segments are replayed at their place in history.** When history is
converted back into provider messages
(`crates/agents/src/model/convert.rs`), a segment is emitted where it closed,
not appended at the end. A segment that never closed ends the history, because
the run was interrupted mid-response.

**An interrupted tool call gets a result.** A replayed segment can carry a
function call the run never finished. Both Chat Completions and Responses reject
a request whose assistant message has a call without a matching result, so
`ensure_tool_call_results_present()`
(`crates/agents/src/model/aborted_calls.rs`) records the missing result as
`aborted` and logs a warning naming the call. The call itself is kept.

**Reasoning survives the round trip.** A replayed reasoning item is serialized
with its summary parts and its opaque `encrypted_content`
(`crates/providers/src/openai_compat/provider.rs`). A missing opaque state is
sent as `null`; it is never replaced by a substitute value.

### 7. Item Identity and Position on Ingress

Every item receives its canonical position exactly once, when its identity first
appears on the stream, from `ItemPositionAllocator`
(`crates/common/src/item_positions.rs`). Transports are not a reliable source of
that slot:

- Chat Completions has no output items at all; reasoning, visible text, and tool
    calls arrive as parallel delta channels with no index ordering them against
    each other.
- Responses carries `output_index` on delta events, but for some providers it
    orders items only within a channel, so two distinct items can share one value
    inside a single response.
- External agents report text and thinking as separate event kinds with no
    ordering between them.

Adapters for those transports assign the identity of the synthesized item once
per segment and take its position from the allocator. Deriving a position from a
transport field would make two items collide on one slot, which the materializer
rejects as a position/id conflict.

A Responses reasoning item can deliver its summary twice: as
`reasoning_summary_text.delta` / `reasoning_summary_part.done` events, and again
in full inside the final `output_item.done`. Parts already received from the
stream are not emitted a second time, so a provider that streams the summary and
one that sends it only at the end both end up with the same segment.

### 8. Web Crate (`crates/web/`)

The `chelix-web` crate owns the browser-facing layer: HTML templates, static
assets (JS, CSS, icons), and the axum routes that serve them. It injects its
routes into the gateway via the `RouteEnhancer` composition pattern, keeping web
UI concerns separate from API and agent logic in the gateway.

### 9. Frontend (`crates/web/ui/src/`)

The TypeScript frontend receives semantic history through
`sessions/history-subscription.ts` and `stores/session-history-cache.ts`.

1. `sessions.history.subscribe` registers the watch receiver before reading its
   baseline page. The response contains the subscription ID and snapshot.
2. `ui_history` events carry updated snapshots with a generation and revision
   watermark. The browser buffers events arriving before the RPC baseline.
3. The reducer applies newer revisions by message ID. A revision gap triggers a
   new baseline; the server recovers a lagged subscriber with a snapshot of its
   selected range.
4. `sessions/session-render.ts` reconciles keyed DOM nodes in server position
   order, preserving the reasoning disclosure of a retained assistant node.
5. `ws/chat-handlers.ts` handles run, queue, voice, and compaction status.

Provider items within a snapshot retain their canonical positions. Tool cards
receive accumulated arguments even when a client subscribes midway through the
parameter stream. All reasoning items of a segment share one disclosure in both
live rendering and history reload.

## Data Flow

```text
Provider stream → runner / stream-only / external-agent ingress
  → UiHistoryEngine → revision watch → per-client delivery → WebSocket → browser
       └→ coalesced SQLite snapshots
  → ordered canonical append → JSONL → next provider request
       └→ canonical receipt → UiHistoryEngine
```

The engine's range reader merges persisted snapshots with dirty and active
entries. `Latest`, `Before`, `After`, `Around`, and `Window` select semantic
positions or an exact message ID. Clear and truncation rotate the generation;
subscribers receive a new baseline.

## Adding Streaming to New Providers

To add streaming support for a new LLM provider:

1. Implement the `stream()` method (basic streaming)
2. If the provider supports tools in streaming mode, override
   `stream_with_tools()`
3. Parse the provider's streaming format and yield appropriate `StreamEvent`
   variants
4. Handle errors gracefully with `StreamEvent::Error`
5. Always emit `StreamEvent::Done` with usage statistics when complete

## Performance Considerations

- **Per-client history delivery**: `ui_history_subscription.rs` reserves space
  in the client's one-frame history queue before reading the latest watch
  revision. Socket waiting stays in that client's task.
- **Coalesced status**: `ChatStatusOutbox` keeps the latest session/run, queue,
  and compaction status independently of history delivery.
- **Bounded browser window**: `session-history-cache.ts` retains up to 240
  snapshots and trims by serialized size at 12 MiB, keeping at least one
  snapshot. Loading older or newer pages evicts the opposite edge.
- **Markdown rendering**: `session-render.ts` updates an assistant's text body
  while retaining its keyed message container and reasoning disclosure.
