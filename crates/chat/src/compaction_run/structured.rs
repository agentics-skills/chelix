//! `CompactionMode::Structured` — head + LLM structured-summary + tail.
//!
//! Same head/tail boundary logic as `recency_preserving`, but the middle
//! region is summarised with a single LLM call using a comprehensive
//! structured template (Conversation Overview / Technical Foundation /
//! Codebase Status / Problem Resolution / Progress Tracking / Active Work
//! State / Recent Operations / Continuation Plan). Iterative re-compaction
//! detects a previous compaction summary in the head and asks the model to
//! preserve and update it instead of re-summarising.
//!
//! On LLM failure (stream error or empty summary), automatically falls
//! back to `recency_preserving` so compaction never silently drops
//! information.
//!
//! The detailed-summary prompt is modelled on GitHub Copilot's agent
//! conversation-history summarizer (`comprehensive, detailed summary ...
//! without any loss of context`): it deliberately does NOT cap the summary
//! length, since aggressive token targets are what cause multi-hour work to
//! be compressed into a lossy stub.

use {
    moltis_agents::model::{ChatMessage, LlmProvider, StreamEvent, Usage, values_to_chat_messages},
    moltis_config::{CompactionConfig, CompactionMode},
    serde_json::Value,
    tokio_stream::StreamExt,
    tracing::info,
};

use super::{
    CompactionOutcome, CompactionRunError, recency_preserving,
    shared::{
        HeadTailBounds, build_summary_message, compute_boundaries, finalize_kept,
        in_place_prune_or_err,
    },
};

/// Structured-summary template used by [`run`].
///
/// An eight-section comprehensive handoff template adapted from GitHub
/// Copilot's agent history summarizer: Conversation Overview, Technical
/// Foundation, Codebase Status, Problem Resolution, Progress Tracking,
/// Active Work State, Recent Operations, and Continuation Plan. Kept
/// verbatim here so future edits are easy to diff and test fixtures can
/// match against the literal template.
const STRUCTURED_TEMPLATE: &str = "\
## 1. Conversation Overview
- Primary Objectives: [all explicit user requests and overarching goals, \
with exact quotes]
- Session Context: [high-level narrative of the conversation flow and key \
phases]
- User Intent Evolution: [how the user's needs or direction changed \
throughout the conversation]

## 2. Technical Foundation
[Each core technology, framework, library, architectural pattern, and \
environment detail — with version/configuration and the purpose or \
reasoning behind it. One item per line.]

## 3. Codebase Status
[For every file read, created, or modified — list each with its full path \
and, indented beneath it:
  - Purpose: why this file matters to the work
  - Current State: what was read, changed, or still needs changing
  - Key Code Segments: important functions/classes, with verbatim snippets \
where they matter and line numbers when known
  - Dependencies: how it relates to other components]

## 4. Problem Resolution
- Issues Encountered: [technical problems, bugs, or challenges — include \
error messages verbatim]
- Solutions Implemented: [how each problem was resolved, and the reasoning]
- Debugging Context: [ongoing troubleshooting efforts or known issues]
- Lessons Learned: [important insights or patterns discovered]

## 5. Progress Tracking
- Completed Tasks: [what is successfully implemented, with status]
- Partially Complete Work: [tasks in progress with current completion \
status]
- Validated Outcomes: [features or code confirmed working, and exactly how \
it was verified — commands run and their output]

## 6. Active Work State
- Current Focus: [precisely what was being worked on in the most recent \
messages]
- Recent Context: [detailed description of the last few exchanges]
- Working Code: [code snippets being modified or discussed recently, \
verbatim]
- Immediate Context: [the specific problem or feature being addressed right \
before this summary]

## 7. Recent Operations
- Last Commands: [specific tools/actions executed just before \
summarization, with exact command names and arguments]
- Tool Results Summary: [key outcomes from recent tool executions — \
truncate very long results but keep the essential information]
- Pre-Summary State: [what was actively being done when the context budget \
was reached]
- Operation Context: [why these commands were run and how they relate to \
the user's goals]

## 8. Continuation Plan
[Each pending task with specific, concrete next steps and verbatim quotes \
from recent messages; which tasks are most urgent or logically sequential; \
and the immediate next action to take]";

/// System-message instructions that frame the structured summary call.
const STRUCTURED_SYSTEM_INSTRUCTIONS: &str = "\
You are a conversation summarizer for an agentic coding session. Your task \
is to create a comprehensive, detailed summary of the conversation that \
captures all essential information needed to seamlessly continue the work \
without any loss of context. This summary compacts the conversation while \
preserving critical technical details, decisions, and progress.\n\
\n\
Pay special attention to the most recent commands and tool executions that \
led to this summarization being triggered: the last commands run, the key \
tool results (truncate very long output but preserve essential \
information), the immediate state, and what was being worked on right \
before the context budget was reached.\n\
\n\
Before producing the final summary, work through the conversation \
systematically: a chronological review of phases and transitions; map \
every explicit and implicit user request, goal, and expectation; catalog \
all technical concepts, tools, frameworks, and architectural decisions; \
document every file, function, and code pattern discussed or modified; \
assess what is done versus what remains pending; and verify that all \
context required for continuation is captured.\n\
\n\
Quality guidelines: be precise (exact filenames, function names, variable \
names, technical terms); be complete (capture everything needed to \
continue without re-reading the full conversation); be clear (write for \
someone picking up exactly where the work left off); be verbatim (use \
direct quotes for task specifications and recent work, and preserve all \
opaque identifiers exactly as written — UUIDs, hashes, tokens, API keys, \
hostnames, IPs, ports, URLs, and file paths — never shorten or \
reconstruct them); and include enough technical depth for complex \
decisions and code patterns.\n\
\n\
Do NOT abbreviate to save space. This is a comprehensive handoff document; \
err on the side of including more detail, not less. After the conversation, \
you will receive a final instruction telling you which template to fill \
in.";

/// User-message instructions for the first compaction of a session.
fn first_compaction_instructions() -> String {
    format!(
        "Produce a comprehensive, detailed handoff summary for a later \
         assistant that will continue this conversation after the earlier \
         turns above are compacted. Fill in every section of the structure \
         below; write \"(none)\" only if a section genuinely has nothing to \
         report.\n\n\
         Pay special attention to the most recent commands and tool results, \
         what was being worked on when the context budget was reached, and \
         how those recent operations connect to the overall goals.\n\n\
         {STRUCTURED_TEMPLATE}\n\n\
         Be exhaustive and detailed — include exact file paths, command \
         outputs, error messages verbatim, code snippets, configuration \
         values, and concrete values rather than vague descriptions. Do not \
         omit information to save space; the goal is zero loss of context so \
         the work can continue seamlessly. Write only the summary body, with \
         no preamble or prefix."
    )
}

/// User-message instructions for iterative re-compaction (a previous
/// summary exists in the first message of the history).
fn iterative_instructions(previous_summary: &str) -> String {
    format!(
        "You are updating a previous compaction summary. The first message in \
         the conversation above is a previous compaction's structured summary; \
         the remaining messages are new turns that must be incorporated.\n\n\
         PREVIOUS SUMMARY:\n{previous_summary}\n\n\
         Update the summary using the structure below. PRESERVE all existing \
         information that is still relevant — do not drop detail from the \
         previous summary unless it is clearly obsolete. ADD new progress with \
         full detail, and move items from in-progress to done when \
         completed.\n\n{STRUCTURED_TEMPLATE}\n\n\
         Be exhaustive and detailed — include exact file paths, command \
         outputs, error messages verbatim, code snippets, configuration \
         values, and concrete values. Do not omit information to save space; \
         the goal is zero loss of context. Write only the summary body, with \
         no preamble or prefix."
    )
}

/// Default value of `max_summary_tokens` the user can leave untouched.
/// Mirrors `default_compaction_max_summary_tokens` in `moltis_config::schema`
/// so we can detect when the user has explicitly set something different.
const DEFAULT_MAX_SUMMARY_TOKENS: u32 = 8_192;

/// State shared across runs so the "summary_model is not wired yet"
/// warning is emitted at most once per configuration, not on every
/// compaction. Without this guard a long session that compacts ten
/// times would spam the log ten times with the same notice.
#[allow(clippy::type_complexity)]
static WARNED_UNUSED_AUXILIARY_CONFIG: std::sync::OnceLock<
    std::sync::Mutex<Option<(Option<String>, u32)>>,
> = std::sync::OnceLock::new();

/// Emit a one-shot runtime WARN when the user has set `summary_model`
/// or a non-default `max_summary_tokens` but the `structured` strategy
/// doesn't wire them yet.
///
/// Until that ships, `structured` always uses the session's primary
/// provider regardless of these fields. Users who configured a cheap
/// auxiliary model (e.g. "openrouter/google/gemini-2.5-flash") would
/// otherwise silently fall through to the frontier model they use for
/// coding, with a nasty billing surprise. The warning names the exact
/// fields and the tracking issue so operators can either disable the
/// config or wait for the feature to land.
///
/// The one-shot guard is keyed on the (model, max_tokens) tuple so
/// mid-session config reloads that change the values re-emit the
/// warning.
fn warn_if_unused_auxiliary_model_config(config: &CompactionConfig) {
    let model_set = config.summary_model.is_some();
    let tokens_overridden = config.max_summary_tokens != DEFAULT_MAX_SUMMARY_TOKENS;
    if !(model_set || tokens_overridden) {
        return;
    }

    let state = WARNED_UNUSED_AUXILIARY_CONFIG.get_or_init(Default::default);
    let key = (config.summary_model.clone(), config.max_summary_tokens);
    let mut guard = match state.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    if guard.as_ref() == Some(&key) {
        return;
    }
    *guard = Some(key);
    drop(guard);

    tracing::warn!(
        summary_model = ?config.summary_model,
        max_summary_tokens = config.max_summary_tokens,
        "chat.compact: chat.compaction.summary_model / max_summary_tokens are not wired \
         into the structured strategy yet — the session's primary provider will be used. \
         Use `[auxiliary] compaction = \"model-id\"` for auxiliary model routing once wired"
    );
}

/// Extract the body of the most recent previous-compaction summary in
/// `history`, if any exists.
///
/// Scans the entire history **in reverse** so iterative re-compaction
/// picks up the newest summary regardless of where it lives. Structured
/// mode splices the new summary at `head_end`, not index 0, so an older
/// check that looked only at `history[0]` never fired for
/// `structured → structured` chains (Greptile P2 on commit 0531913b).
///
/// Only matches user messages whose content starts with
/// `[Conversation Summary]\n\n` — the prefix produced by every mode
/// that wraps its output via [`build_summary_message`]. Recency-
/// preserving's `[Conversation Compacted]` middle markers are
/// intentionally ignored: they're not summaries, just elision notices,
/// and feeding them back into the LLM as "previous summary" context
/// would confuse the re-compaction prompt.
///
/// [`build_summary_message`]: super::shared::build_summary_message
fn extract_previous_summary(history: &[Value]) -> Option<&str> {
    history.iter().rev().find_map(|msg| {
        if msg.get("role").and_then(Value::as_str) != Some("user") {
            return None;
        }
        msg.get("content")
            .and_then(Value::as_str)?
            .strip_prefix("[Conversation Summary]\n\n")
    })
}

/// Run the structured LLM-summary strategy against `history`.
///
/// Falls back to [`recency_preserving::run`] on LLM stream error or
/// empty summary, so compaction never silently drops information. When
/// the fallback fires, the returned outcome reports
/// `effective_mode = CompactionMode::RecencyPreserving` so the UI can
/// accurately show what actually ran.
pub(super) async fn run(
    history: &[Value],
    config: &CompactionConfig,
    context_window: u32,
    provider: &dyn LlmProvider,
) -> Result<CompactionOutcome, CompactionRunError> {
    // Warn once if the user configured `summary_model` or a non-default
    // `max_summary_tokens`: those fields are reserved for the auxiliary
    // model subsystem and will not affect this run.
    warn_if_unused_auxiliary_model_config(config);

    let bounds = compute_boundaries(history, config, context_window);
    let HeadTailBounds {
        head_end,
        tail_start,
        protect_tail_min,
        ..
    } = bounds;
    let n = history.len();

    // Head and tail already cover everything — no middle to summarise.
    // After this guard, `head_end < tail_start` is guaranteed, so the
    // slice below is always non-empty.
    if head_end >= tail_start {
        let kept = in_place_prune_or_err(history, config, &bounds)?;
        return Ok(CompactionOutcome {
            history: kept,
            effective_mode: CompactionMode::Structured,
            input_tokens: 0,
            output_tokens: 0,
        });
    }

    let middle = &history[head_end..tail_start];

    // Detect re-compaction: if any message in the history looks like a
    // previous compaction summary, include it in the prompt so the model
    // can update sections instead of re-summarising from scratch.
    // Scanning the full history (not just the head) is critical for
    // `structured → structured` chains where the previous summary lives
    // at `head_end`, inside the middle region we're about to re-summarise.
    let previous_summary = extract_previous_summary(history);

    // Build the structured prompt. System message frames the task, middle
    // messages are passed via ChatMessage so role boundaries are preserved
    // (prevents prompt injection via role prefixes in user content), and a
    // final user directive selects the template.
    let mut summary_messages = vec![ChatMessage::system(STRUCTURED_SYSTEM_INSTRUCTIONS)];
    summary_messages.extend(values_to_chat_messages(middle));
    summary_messages.push(match previous_summary {
        Some(prev) => ChatMessage::user(iterative_instructions(prev)),
        None => ChatMessage::user(first_compaction_instructions()),
    });

    // Stream the summary, capturing both the text body and the final
    // Usage report from the provider so we can surface token counts in
    // the compaction broadcast.
    let mut stream = provider.stream(summary_messages);
    let mut summary = String::new();
    let mut usage = Usage::default();
    let mut stream_error: Option<String> = None;
    while let Some(event) = stream.next().await {
        match event {
            StreamEvent::Delta(delta) => summary.push_str(&delta),
            StreamEvent::Done(u) => {
                usage = u;
                break;
            },
            StreamEvent::Error(e) => {
                stream_error = Some(e.to_string());
                break;
            },
            // Tool events aren't expected on a summary stream; drop them.
            StreamEvent::ToolCallStart { .. }
            | StreamEvent::ToolCallArgumentsDelta { .. }
            | StreamEvent::ToolCallComplete { .. }
            // Provider raw payloads are debug metadata, not summary text.
            | StreamEvent::ProviderRaw(_)
            // Ignore reasoning blocks; the summary body is the final answer only.
            | StreamEvent::ReasoningDelta(_) => {},
        }
    }

    // `config.max_summary_tokens` / `config.summary_model` aren't wired
    // into the provider stream yet. The warn-on-configured check runs at
    // the top of this function so users don't silently get default
    // behaviour when they expected a cheaper auxiliary model.
    let _ = config.max_summary_tokens;
    let _ = config.summary_model.as_deref();

    if let Some(err) = stream_error {
        tracing::warn!(
            error = %err,
            "chat.compact: structured summary stream failed, falling back to recency_preserving"
        );
        return recency_preserving::run(history, config, context_window);
    }
    let summary = summary.trim();
    if summary.is_empty() {
        tracing::warn!(
            "chat.compact: structured summary was empty, falling back to recency_preserving"
        );
        return recency_preserving::run(history, config, context_window);
    }

    // Assemble head + structured-summary + tail.
    let mut kept: Vec<Value> = Vec::with_capacity(head_end + 1 + (n - tail_start));
    kept.extend(history[..head_end].iter().cloned());
    kept.push(build_summary_message(summary));
    kept.extend(history[tail_start..].iter().cloned());

    let kept = finalize_kept(kept, config, protect_tail_min)?;

    info!(
        input_messages = n,
        output_messages = kept.len(),
        head = head_end,
        middle = tail_start - head_end,
        tail = n - tail_start,
        summary_chars = summary.len(),
        input_tokens = usage.input_tokens,
        output_tokens = usage.output_tokens,
        iterative = previous_summary.is_some(),
        "chat.compact: structured"
    );

    Ok(CompactionOutcome {
        history: kept,
        effective_mode: CompactionMode::Structured,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use {
        super::{super::test_support::StubProvider, *},
        moltis_config::CompactionMode,
        serde_json::json,
    };

    fn mk_user(text: &str) -> Value {
        json!({"role": "user", "content": text})
    }

    fn mk_assistant(text: &str) -> Value {
        json!({"role": "assistant", "content": text})
    }

    fn sample_history() -> Vec<Value> {
        vec![
            mk_user("hello"),
            mk_assistant("hi there"),
            mk_user("what is 2+2"),
            mk_assistant("4"),
        ]
    }

    #[tokio::test]
    async fn structured_mode_without_provider_returns_provider_required() {
        let history = sample_history();
        let config = CompactionConfig {
            mode: CompactionMode::Structured,
            ..Default::default()
        };
        let err = super::super::run_compaction(&history, &config, None)
            .await
            .unwrap_err();
        match err {
            CompactionRunError::ProviderRequired { mode } => assert_eq!(mode, "structured"),
            other => panic!("expected ProviderRequired, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn structured_mode_splices_summary_between_head_and_tail() {
        let mut history = Vec::new();
        for i in 0..5 {
            history.push(mk_user(&format!("user {i}")));
            history.push(mk_assistant(&format!("assistant {i}")));
        }

        let config = CompactionConfig {
            mode: CompactionMode::Structured,
            protect_head: 2,
            protect_tail_min: 2,
            ..Default::default()
        };
        let provider =
            StubProvider::new_ok("## Goal\nTest compaction\n## Progress\n### Done\nAll the things");
        let outcome = super::super::run_compaction(&history, &config, Some(&provider))
            .await
            .expect("structured succeeds with stub provider");

        assert_eq!(outcome.effective_mode, CompactionMode::Structured);
        let result = outcome.history;

        // Head (2) + structured summary (1) + tail (2) = 5 messages.
        assert_eq!(result.len(), 5, "result: {result:#?}");

        assert_eq!(
            result[0].get("content").and_then(Value::as_str),
            Some("user 0")
        );
        assert_eq!(
            result[1].get("content").and_then(Value::as_str),
            Some("assistant 0")
        );

        let summary = result[2]
            .get("content")
            .and_then(Value::as_str)
            .expect("summary");
        assert!(
            summary.starts_with("[Conversation Summary]\n\n"),
            "got: {summary}"
        );
        assert!(summary.contains("## Goal"), "got: {summary}");

        assert_eq!(
            result[3].get("content").and_then(Value::as_str),
            Some("user 4")
        );
        assert_eq!(
            result[4].get("content").and_then(Value::as_str),
            Some("assistant 4")
        );
    }

    #[tokio::test]
    async fn structured_mode_forwards_previous_summary_on_recompaction() {
        // First head message is a previous compaction summary. The stub
        // provider captures whether any forwarded message contains the
        // unique needle from that prior body, verifying that the
        // iterative-compaction prompt actually reaches the provider.
        const NEEDLE: &str = "previous-compaction-needle-a1b2c3";
        let prior = format!("[Conversation Summary]\n\n## Goal\n{NEEDLE}");
        let mut history = vec![
            json!({"role": "user", "content": prior}),
            mk_assistant("ok got it"),
        ];
        for i in 0..5 {
            history.push(mk_user(&format!("user {i}")));
            history.push(mk_assistant(&format!("assistant {i}")));
        }

        let config = CompactionConfig {
            mode: CompactionMode::Structured,
            protect_head: 2,
            protect_tail_min: 2,
            ..Default::default()
        };
        let provider = StubProvider::new_ok("## Goal\nstub output").with_needle(NEEDLE);
        let _ = super::super::run_compaction(&history, &config, Some(&provider))
            .await
            .expect("structured succeeds with stub provider");

        assert!(
            provider.saw_needle(),
            "structured mode must forward the previous summary body into the iterative-compaction prompt"
        );
    }

    #[tokio::test]
    async fn structured_mode_falls_back_to_recency_preserving_on_llm_error() {
        let mut history = Vec::new();
        for i in 0..5 {
            history.push(mk_user(&format!("user {i}")));
            history.push(mk_assistant(&format!("assistant {i}")));
        }

        let config = CompactionConfig {
            mode: CompactionMode::Structured,
            protect_head: 2,
            protect_tail_min: 2,
            ..Default::default()
        };
        let provider = StubProvider::new_error("simulated provider outage");
        let outcome = super::super::run_compaction(&history, &config, Some(&provider))
            .await
            .expect("structured falls back to recency_preserving on llm error");

        // The outcome reports the effective mode — the UI can use this to
        // tell the user that the requested structured mode fell back to
        // recency_preserving.
        assert_eq!(outcome.effective_mode, CompactionMode::RecencyPreserving);
        let result = outcome.history;

        // Fallback produces a recency_preserving-shaped history: head (2) +
        // middle marker (1) + tail (2) = 5 messages, and the middle message
        // is the plain "[Conversation Compacted]" marker, not a structured
        // summary.
        assert_eq!(result.len(), 5, "result: {result:#?}");
        let middle = result[2]
            .get("content")
            .and_then(Value::as_str)
            .expect("middle content");
        assert!(
            middle.starts_with("[Conversation Compacted]"),
            "fallback should produce the recency_preserving marker, got: {middle}"
        );
    }

    #[tokio::test]
    async fn structured_mode_falls_back_when_summary_is_empty() {
        // A stream that yields Done with no Delta should surface as an
        // empty summary and trigger the same fallback path as an error.
        let mut history = Vec::new();
        for i in 0..5 {
            history.push(mk_user(&format!("user {i}")));
            history.push(mk_assistant(&format!("assistant {i}")));
        }

        let config = CompactionConfig {
            mode: CompactionMode::Structured,
            protect_head: 2,
            protect_tail_min: 2,
            ..Default::default()
        };
        let provider = StubProvider::new_empty_summary();
        let outcome = super::super::run_compaction(&history, &config, Some(&provider))
            .await
            .expect("structured falls back on empty summary");
        assert_eq!(outcome.effective_mode, CompactionMode::RecencyPreserving);
        let result = outcome.history;
        assert_eq!(result.len(), 5);
        let middle = result[2]
            .get("content")
            .and_then(Value::as_str)
            .expect("middle content");
        assert!(
            middle.starts_with("[Conversation Compacted]"),
            "expected fallback marker, got: {middle}"
        );
    }

    #[test]
    fn extract_previous_summary_detects_compacted_head() {
        let history = vec![json!({
            "role": "user",
            "content": "[Conversation Summary]\n\n## Goal\nprior goal",
        })];
        assert_eq!(
            extract_previous_summary(&history),
            Some("## Goal\nprior goal")
        );

        let not_compacted = vec![json!({"role": "user", "content": "hello"})];
        assert_eq!(extract_previous_summary(&not_compacted), None);

        let empty: Vec<Value> = Vec::new();
        assert_eq!(extract_previous_summary(&empty), None);
    }

    #[test]
    fn extract_previous_summary_finds_summary_in_middle_of_history() {
        // After a prior structured compaction, the summary lives at
        // `head_end` (not index 0). Regression test for Greptile P2 on
        // commit 0531913b — `extract_previous_summary` used to scan
        // only `history[..head_end]` and never find it, so iterative
        // structured→structured re-compaction silently fell through to
        // first-compaction mode.
        let history = vec![
            json!({"role": "user", "content": "first user turn"}),
            json!({"role": "assistant", "content": "first assistant reply"}),
            json!({"role": "user", "content": "second user turn"}),
            json!({
                "role": "user",
                "content": "[Conversation Summary]\n\n## Goal\nprior goal body",
            }),
            json!({"role": "user", "content": "newer user turn"}),
            json!({"role": "assistant", "content": "newer assistant reply"}),
        ];
        assert_eq!(
            extract_previous_summary(&history),
            Some("## Goal\nprior goal body"),
            "should find the previous summary at index 3 even though protect_head=3"
        );
    }

    #[test]
    fn extract_previous_summary_picks_newest_when_multiple_exist() {
        // Defensive: if multiple prior summaries somehow survive in the
        // history (e.g. a user pasted an older one into a message), the
        // reverse walk picks the most recent. This matches
        // `compaction_run::extract_summary_body` for the memory-file
        // snapshot.
        let history = vec![
            json!({
                "role": "user",
                "content": "[Conversation Summary]\n\n## Goal\nold body",
            }),
            json!({"role": "user", "content": "recent user turn"}),
            json!({
                "role": "user",
                "content": "[Conversation Summary]\n\n## Goal\nnew body",
            }),
        ];
        assert_eq!(
            extract_previous_summary(&history),
            Some("## Goal\nnew body")
        );
    }

    #[test]
    fn extract_previous_summary_ignores_conversation_compacted_markers() {
        // `[Conversation Compacted]` is the recency_preserving middle
        // marker, not a real summary. Feeding it back into the LLM as
        // "previous summary" context would confuse the re-compaction
        // prompt. Only `[Conversation Summary]` should match.
        let history = vec![
            json!({
                "role": "user",
                "content": "[Conversation Compacted]\n\n6 earlier messages elided",
            }),
            json!({"role": "user", "content": "recent user turn"}),
        ];
        assert_eq!(extract_previous_summary(&history), None);
    }
}
