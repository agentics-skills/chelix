# Streaming Architecture

This document explains how streaming responses work in Chelix, from the LLM
provider through to the web UI.

## Overview

Chelix supports real-time token streaming for LLM responses, providing a much
better user experience than waiting for the complete response. Streaming works
even when tools are enabled, allowing users to see text as it arrives while tool
calls are accumulated and executed.

## Components

### 1. StreamEvent Enum (`crates/agents/src/model.rs`)

The `StreamEvent` enum defines all events that can occur during a streaming LLM
response:

```rust
pub enum StreamEvent {
    /// Text content delta.
    Delta(String),

    /// Raw provider event payload (for debugging API responses).
    ProviderRaw(serde_json::Value),

    /// Reasoning/planning text delta (not user-visible final answer text).
    ReasoningDelta(String),

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

### 2. LlmProvider Trait (`crates/agents/src/model.rs`)

The `LlmProvider` trait defines two streaming methods:

- `stream()` — Basic streaming without tool support
- `stream_with_tools()` — Streaming with tool schemas passed to the API

Both accept `Vec<ChatMessage>` (not raw JSON). Providers that support streaming
with tools override `stream_with_tools()`. Others fall back to `stream()` via
the default implementation, which ignores the tools parameter.

The trait also exposes `supports_tools()`, `reasoning_effort()`, and
`with_reasoning_effort()` for provider capability discovery.

### 3. Agent Runner (`crates/agents/src/runner/streaming.rs`)

The `run_agent_loop_streaming()` function orchestrates the streaming agent loop:

```
┌──────────────────────────────────────────────────────────────────┐
│                         Agent Loop                               │
│                                                                  │
│  1. Call provider.stream_with_tools()                            │
│                                                                  │
│  2. While the provider stream has events:                        │
│     ├─ Delta(text) → emit RunnerEvent::TextDelta                 │
│     ├─ ToolCallStart → emit Created and accumulate the call      │
│     ├─ ToolCallArgumentsDelta → emit InputStreaming              │
│     ├─ ToolCallComplete → mark arguments complete                │
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

### 4. Chat Service (`crates/chat/src/run_with_tools.rs`)

The chat service's `run_with_tools()` function:

1. Sets up the ordinary `RunnerEvent` callback for text, reasoning, notices, and
   other non-tool stream updates.
2. Sets up a separate ordered `OnToolLifecycle` callback. `input_streaming`
   returns after enqueue so provider argument streaming never waits for
   persistence. Every other stage waits for its processor receipt and therefore
   forms an authoritative boundary after all earlier updates.
3. Calls `run_agent_loop_streaming()` from
   `crates/agents/src/runner/streaming.rs`.
4. Broadcasts every lifecycle update. Adjacent input deltas are accumulated and
   persisted as one `input_streaming` checkpoint immediately before that
   invocation's next non-input boundary; each non-input update is persisted
   before its WebSocket frame is broadcast.

Ordinary runner-event handling in the chat service:

| RunnerEvent                         | Chat service handling                                      |
| ----------------------------------- | ---------------------------------------------------------- |
| `Thinking`                          | Broadcasts `thinking`                                      |
| `ThinkingDone`                      | Broadcasts `thinking_done`                                 |
| `ThinkingText(text)`                | Broadcasts `thinking_text`                                 |
| `ResponsesReasoningDelta { ... }`   | Broadcasts `thinking_text` with accumulated source parts   |
| `ResponsesReasoningPartDone { ... }` | Broadcasts `thinking_text` with the authoritative part text |
| `ResponsesReasoningItem(item)`      | Persists backend-only opaque replay state; not broadcast    |
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

### 5. Web Crate (`crates/web/`)

The `chelix-web` crate owns the browser-facing layer: HTML templates, static
assets (JS, CSS, icons), and the axum routes that serve them. It injects its
routes into the gateway via the `RouteEnhancer` composition pattern, keeping web
UI concerns separate from API and agent logic in the gateway.

### 6. Frontend (`crates/web/ui/src/`)

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

When a `delta` event arrives:

```javascript
function handleChatDelta(p, isActive, isChatPage) {
  if (!(p.text && isActive && isChatPage)) return;
  removeThinking();
  if (!S.streamEl) {
    S.setStreamText("");
    S.setStreamEl(document.createElement("div"));
    S.streamEl.className = "msg assistant";
    S.chatMsgBox.appendChild(S.streamEl);
  }
  S.setStreamText(S.streamText + p.text);
  setSafeMarkdownHtml(S.streamEl, S.streamText);
  S.chatMsgBox.scrollTop = S.chatMsgBox.scrollHeight;
}
```

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
