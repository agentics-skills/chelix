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

The `LlmProvider` trait defines two streaming methods:

- `stream()` — Basic streaming without tool support
- `stream_with_tools()` — Streaming with tool schemas passed to the API

Both accept `Vec<ChatMessage>` (not raw JSON). Providers that support streaming
with tools override `stream_with_tools()`. Others fall back to `stream()` via
the default implementation, which ignores the tools parameter.

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

The chat service's `run_with_tools()` function:

1. Sets up the ordinary `RunnerEvent` callback for text, reasoning, notices, and
   other non-tool stream updates.
2. Sets up a separate ordered `OnToolLifecycle` callback. `input_streaming`
   returns after enqueue so provider argument streaming never waits for
   persistence. Every other stage waits for its processor receipt and therefore
   forms an authoritative boundary after all earlier updates.
3. Calls `run_agent_loop_streaming()` from
   `crates/agents/src/runner/streaming.rs`.
4. Persists updates to the session store first, obtains the authoritative
   `historyIndex`, and broadcasts the public redacted record to connected clients.

Ordinary runner-event handling in the chat service:

| RunnerEvent                         | Chat service handling                                      |
| ----------------------------------- | ---------------------------------------------------------- |
| `SegmentStart`                      | Broadcasts `segment_start`                                 |
| `ProviderItemUpdate(update)`        | Persists `provider_update` and broadcasts with `historyIndex` |
| `SegmentClose`                      | Persists `provider_segment_close` and broadcasts with `historyIndex` |
| `Thinking`                          | Broadcasts `thinking`                                      |
| `ThinkingDone`                      | Broadcasts `thinking_done`                                 |
| `TextDelta(text)`                   | Broadcasts `delta` with `text` field                       |
| `Iteration(n)`                      | Broadcasts `iteration`                                     |
| `SubAgentStart`                     | Broadcasts `sub_agent_start`                               |
| `SubAgentEnd`                       | Broadcasts `sub_agent_end`                                 |
| `AutoContinue`                      | Broadcasts `notice` ("Auto-continue")                      |
| `RetryingAfterError`                | Broadcasts `retrying`                                      |
| `LoopInterventionFired`             | Broadcasts `notice` ("Loop detected")                      |

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

Persisted lifecycle records use `role: "tool_lifecycle"` and store their `runId`,
per-call `sequence`, and `emittedAtMs` timestamp in the lifecycle event itself.
Terminal events can also carry `contextBudget`. Non-terminal events replace the
active invocation snapshot, whose separate `accumulatedArguments` field retains
streamed input; terminal events remove it. Lifecycle records do not reserve or
broadcast a physical `messageIndex`. The WebSocket envelope adds `sessionKey`
and, at `input_ready`, the canonical `assistantMessage` and the
`assistantMessageIndex` returned by its successful append.

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

The TypeScript frontend handles streaming via WebSocket:

1. **websocket.ts** receives WebSocket frames and dispatches them to handlers.
2. **ws/chat-handlers.ts** accepts `state: "tool_lifecycle"` frames and upserts
   one cached lifecycle entity per `(runId, toolCallId)`; only the first frame for
   that invocation increments the logical session count.
3. **tool-lifecycle.ts** validates the discriminated lifecycle union, accumulates
   input deltas outside the wire event, and reduces per-call snapshots by
   sequence. Terminal result strings are JSON-decoded only by consumers that
   need structured fields.
4. **ws/tool-helpers.ts** applies snapshots to one invocation card, including
   backend-authored progress and terminal presentation. Terminal attachment is
   allowed only for interactive live rendering.
5. **sessions/session-render.ts** and **sessions/session-switch.ts** reuse the
   same reducer and renderer for the latest persisted invocation snapshots and
   active reconnect snapshots. History rendering is non-interactive and never
   opens a terminal attachment WebSocket.

`handleChatDelta` in `ws/chat-handlers.ts` appends the text to the live
assistant element, creating it on first use. Rendered markdown lives in a
dedicated wrapper inside that element, so a delta never detaches the reasoning
disclosure or the action bar next to it.

Each segment owns one bubble. `segment_start` for a different segment releases
the current element so the next segment starts its own: an element that produced
nothing visible is removed, and one that produced something is finished in place
and left in the conversation. Without that release, a retry would append the
second attempt to the text of the first.

All reasoning items of a segment render into a single disclosure, as parts of
one reasoning stream. The live view and the view rebuilt from history use the
same aggregation, so a reload shows the same structure as the live run.

## Data Flow

```
┌──────────────┐     SSE      ┌──────────────┐   StreamEvent   ┌──────────────┐
│   LLM API    │─────────────▶│   Provider   │────────────────▶│    Runner    │
│              │              │              │                 │              │
└──────────────┘              └──────────────┘                 └──────┬───────┘
                                                                      │
                                                               RunnerEvent
                                                                      │
                                                                      ▼
┌──────────────┐   WebSocket  ┌──────────────┐   Routes/WS   ┌──────────────┐    Callback     ┌──────────────┐
│   Browser    │◀─────────────│  Web Crate   │◀──────────────│ Chat Service │◀────────────────│   Callback   │
│              │              │  (chelix-web)│               │              │                 │   (on_event) │
└──────────────┘              └──────────────┘               └──────────────┘                 └──────────────┘
```

## Adding Streaming to New Providers

To add streaming support for a new LLM provider:

1. Implement the `stream()` method (basic streaming)
2. If the provider supports tools in streaming mode, override
   `stream_with_tools()`
3. Parse the provider's streaming format and yield appropriate `StreamEvent`
   variants
4. Handle errors gracefully with `StreamEvent::Error`
5. Always emit `StreamEvent::Done` with usage statistics when complete

Example skeleton:

```rust
fn stream_with_tools(
    &self,
    messages: Vec<ChatMessage>,
    _tools: Vec<serde_json::Value>,
) -> Pin<Box<dyn Stream<Item = StreamEvent> + Send + '_>> {
    Box::pin(async_stream::stream! {
        // Make streaming request to provider API
        let resp = self.client.post(...)
            .json(&body)
            .send()
            .await?;

        // Read SSE or streaming response
        let mut byte_stream = resp.bytes_stream();

        while let Some(chunk) = byte_stream.next().await {
            // Parse chunk and yield events
            match parse_event(&chunk) {
                TextDelta(text) => yield StreamEvent::Delta(text),
                ToolStart { id, name, idx } => {
                    yield StreamEvent::ToolCallStart { id, name, index: idx }
                }
                // ... handle other event types
            }
        }

        yield StreamEvent::Done(usage);
    })
}
```

## Performance Considerations

- **Unbounded channels**: WebSocket send channels are unbounded, so slow clients
  can accumulate messages in memory
- **Markdown re-rendering**: The frontend re-renders full markdown on each
  delta, which is O(n) work per delta. For very long responses, this can cause
  UI lag
- **Concurrent tool execution**: Multiple tool calls are executed in parallel
  using `futures::join_all()`, improving throughput when the LLM requests
  several tools at once
