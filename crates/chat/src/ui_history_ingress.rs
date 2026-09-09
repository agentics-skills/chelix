//! Copies runner output into semantic history before journal forwarding.

use std::sync::Arc;

use {
    chelix_agents::runner::{RunnerEvent, RunnerToolLifecycleEvent},
    chelix_sessions::{
        PersistedMessage,
        store::SessionStore,
        ui_history_engine::UiHistoryRun,
        ui_history_types::{UiProviderError, UiRunMetadata},
    },
    tokio_util::sync::CancellationToken,
};

pub(crate) async fn begin(
    store: Option<&Arc<SessionStore>>,
    session_key: &str,
    run_id: &str,
    model: &str,
    provider: &str,
    reasoning_effort: Option<String>,
) -> chelix_sessions::Result<Option<UiHistoryRun>> {
    let Some(store) = store else {
        return Ok(None);
    };
    let session = store.ui_history.session(session_key).await?;
    session
        .begin_run(UiRunMetadata {
            run_id: run_id.to_string(),
            model: model.to_string(),
            provider: provider.to_string(),
            reasoning_effort,
        })
        .map(Some)
}

pub(crate) fn copy_event(run: &UiHistoryRun, event: &RunnerEvent) -> chelix_sessions::Result<()> {
    let message = match event {
        RunnerEvent::Thinking => return run.start_attempt(None),
        RunnerEvent::SegmentStart { segment_id } => {
            return run.start_attempt(Some(segment_id.clone()));
        },
        RunnerEvent::ProviderError {
            error,
            segment_id,
            retry_after_ms,
        } => {
            let metadata = run.metadata()?;
            run.error(UiProviderError {
                run_id: metadata.run_id,
                segment_id: segment_id.clone(),
                created_at: crate::types::now_ms(),
                raw: error.clone(),
                details: crate::chat_error::parse_chat_error(error, Some(&metadata.provider)),
                retry_after_ms: *retry_after_ms,
            })?;
            return Ok(());
        },
        RunnerEvent::ProviderItemUpdate(update) => PersistedMessage::ProviderUpdate {
            update: update.clone(),
            created_at: Some(crate::types::now_ms()),
            seq: None,
            run_id: None,
        },
        RunnerEvent::SegmentClose {
            segment_id,
            outcome,
            ..
        } => PersistedMessage::ProviderSegmentClose {
            segment_id: segment_id.clone(),
            outcome: *outcome,
            created_at: Some(crate::types::now_ms()),
            seq: None,
            run_id: None,
        },
        _ => return Ok(()),
    };
    let id = run.copy(message)?;
    if let RunnerEvent::SegmentClose {
        usage: Some(usage), ..
    } = event
    {
        run.merge_metadata(
            &id,
            std::collections::BTreeMap::from([(
                "segmentUsage".to_string(),
                serde_json::to_value(usage)?,
            )]),
        )?;
    }
    Ok(())
}

pub(crate) fn copy_lifecycle(
    run: &UiHistoryRun,
    event: &RunnerToolLifecycleEvent,
    sandbox_enabled: bool,
) -> chelix_sessions::Result<()> {
    let mut lifecycle = event.lifecycle.clone();
    lifecycle.context_budget = event.context_budget.clone().or(lifecycle.context_budget);
    let execution_mode = tool_execution_mode(&lifecycle.tool_name, sandbox_enabled);
    let id = run.copy(PersistedMessage::ToolLifecycle { lifecycle })?;
    if let Some(presentation) = &event.ui_presentation {
        run.present(&id, presentation.clone())?;
    }
    if let Some(mode) = execution_mode {
        run.merge_metadata(
            &id,
            std::collections::BTreeMap::from([(
                "executionMode".into(),
                serde_json::Value::String(mode),
            )]),
        )?;
    }
    Ok(())
}

pub(crate) fn tool_execution_mode(tool_name: &str, sandbox_enabled: bool) -> Option<String> {
    (tool_name == "browser").then(|| {
        if sandbox_enabled {
            "sandbox".to_string()
        } else {
            "host".to_string()
        }
    })
}

pub(crate) async fn fail_run(
    run: Option<&UiHistoryRun>,
    state: &Arc<dyn crate::runtime::ChatRuntime>,
    run_id: &str,
    raw: String,
    provider: &str,
    details: Option<serde_json::Value>,
) {
    if let Some(run) = run {
        let copied = (|| {
            if run.recorded_error(&raw)?.is_none() {
                run.error(UiProviderError {
                    run_id: run_id.to_string(),
                    segment_id: None,
                    created_at: crate::types::now_ms(),
                    raw: raw.clone(),
                    details: details.unwrap_or_else(|| {
                        crate::chat_error::parse_chat_error(&raw, Some(provider))
                    }),
                    retry_after_ms: None,
                })?;
            }
            Ok::<_, chelix_sessions::Error>(())
        })();
        if let Err(error) = copied {
            tracing::error!(run_id, %error, "failed to retain run error in UI history");
        }
        if let Err(error) = run.flush().await {
            tracing::error!(run_id, %error, "run error UI persistence failed");
            state
                .set_run_error(run_id, format!("{raw}; UI persistence failed: {error}"))
                .await;
            return;
        }
    }
    state.set_run_error(run_id, raw).await;
}

pub(crate) fn record_error(
    run: &UiHistoryRun,
    run_id: &str,
    raw: &str,
    provider: &str,
    retry_after_ms: Option<u64>,
) -> chelix_sessions::Result<()> {
    run.error(UiProviderError {
        run_id: run_id.to_string(),
        segment_id: None,
        created_at: crate::types::now_ms(),
        raw: raw.to_string(),
        details: crate::chat_error::parse_chat_error(raw, Some(provider)),
        retry_after_ms,
    })?;
    Ok(())
}

pub(crate) async fn finish_output(
    run: Option<&UiHistoryRun>,
    output: &crate::types::AssistantTurnOutput,
    medium: crate::types::ReplyMedium,
    audio_warning: Option<String>,
    tools: Option<(usize, usize)>,
) -> chelix_sessions::Result<()> {
    let Some(run) = run else {
        return Ok(());
    };
    let segment_id = output.segment_id.as_ref().ok_or_else(|| {
        chelix_sessions::Error::message("terminal assistant has no provider segment identity")
    })?;
    let mut metadata = std::collections::BTreeMap::from([(
        "replyMedium".to_string(),
        serde_json::to_value(medium)?,
    )]);
    if let Some(warning) = audio_warning {
        metadata.insert(
            "audioWarning".to_string(),
            serde_json::Value::String(warning),
        );
    }
    if let Some((iterations, tool_calls)) = tools {
        metadata.insert("iterations".to_string(), serde_json::json!(iterations));
        metadata.insert("toolCallsMade".to_string(), serde_json::json!(tool_calls));
    }
    run.merge_metadata(
        &chelix_sessions::ui_history_types::UiMessageId::segment(segment_id),
        metadata,
    )?;
    run.flush().await
}

pub(crate) async fn finish(
    run: Option<&UiHistoryRun>,
    outcome: crate::types::ChatRunOutcome,
    state: &Arc<dyn crate::runtime::ChatRuntime>,
    run_id: &str,
) -> crate::types::ChatRunOutcome {
    if let Some(run) = run
        && let Err(error) = run.finish().await
    {
        tracing::error!(%error, run_id, "failed to finish semantic UI history");
        state.set_run_error(run_id, error.to_string()).await;
        return crate::types::ChatRunOutcome::Failed;
    }
    outcome
}

pub(crate) fn monitor(
    run: Option<&UiHistoryRun>,
    cancellation: CancellationToken,
) -> Option<tokio::task::JoinHandle<()>> {
    let mut health = run?.health();
    Some(tokio::spawn(async move {
        loop {
            if health.borrow_and_update().failure.is_some() {
                cancellation.cancel();
                break;
            }
            if health.changed().await.is_err() {
                break;
            }
        }
    }))
}
