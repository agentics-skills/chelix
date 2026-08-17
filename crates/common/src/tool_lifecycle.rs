use {
    crate::ContextBudgetMetadata,
    serde::{Deserialize, Serialize},
    serde_json::Value,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolLifecycleStage {
    Created,
    InputStreaming,
    InputReady,
    WaitingForExecution,
    Executing,
    ExecutionProgress,
    ResultReady,
    Completed,
    Rejected,
    Cancelled,
}

impl ToolLifecycleStage {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Rejected | Self::Cancelled)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolLifecycleEvent {
    pub tool_call_id: String,
    pub tool_name: String,
    pub sequence: u64,
    pub emitted_at_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_budget: Option<ContextBudgetMetadata>,
    #[serde(flatten)]
    pub update: ToolLifecycleUpdate,
}

impl ToolLifecycleEvent {
    #[must_use]
    pub const fn stage(&self) -> ToolLifecycleStage {
        self.update.stage()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "stage",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ToolLifecycleUpdate {
    Created {
        provider_index: Option<usize>,
    },
    InputStreaming {
        arguments_delta: String,
    },
    InputReady {
        arguments: Value,
    },
    WaitingForExecution {
        arguments: Value,
    },
    Executing {
        arguments: Value,
        started_at_ms: u64,
    },
    ExecutionProgress {
        arguments: Value,
        elapsed_ms: u64,
        message: String,
    },
    ResultReady {
        arguments: Value,
        success: bool,
        result: Option<String>,
        error: Option<String>,
    },
    Completed {
        arguments: Value,
        success: bool,
        result: Option<String>,
        error: Option<String>,
    },
    Rejected {
        arguments: Value,
        reason: String,
        result: String,
    },
    Cancelled {
        arguments: Option<Value>,
        reason: String,
    },
}

impl ToolLifecycleUpdate {
    #[must_use]
    pub const fn stage(&self) -> ToolLifecycleStage {
        match self {
            Self::Created { .. } => ToolLifecycleStage::Created,
            Self::InputStreaming { .. } => ToolLifecycleStage::InputStreaming,
            Self::InputReady { .. } => ToolLifecycleStage::InputReady,
            Self::WaitingForExecution { .. } => ToolLifecycleStage::WaitingForExecution,
            Self::Executing { .. } => ToolLifecycleStage::Executing,
            Self::ExecutionProgress { .. } => ToolLifecycleStage::ExecutionProgress,
            Self::ResultReady { .. } => ToolLifecycleStage::ResultReady,
            Self::Completed { .. } => ToolLifecycleStage::Completed,
            Self::Rejected { .. } => ToolLifecycleStage::Rejected,
            Self::Cancelled { .. } => ToolLifecycleStage::Cancelled,
        }
    }
}

/// Latest authoritative state of a tool invocation in an active agent run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveToolInvocation {
    #[serde(flatten)]
    pub lifecycle: ToolLifecycleEvent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accumulated_arguments: Option<String>,
    /// Current budget retained internally so cancellation can publish terminal metadata.
    #[serde(skip)]
    pub context_budget: Option<ContextBudgetMetadata>,
}

impl ActiveToolInvocation {
    #[must_use]
    pub fn arguments(&self) -> Option<&Value> {
        match &self.lifecycle.update {
            ToolLifecycleUpdate::InputReady { arguments }
            | ToolLifecycleUpdate::WaitingForExecution { arguments }
            | ToolLifecycleUpdate::Executing { arguments, .. }
            | ToolLifecycleUpdate::ExecutionProgress { arguments, .. }
            | ToolLifecycleUpdate::ResultReady { arguments, .. }
            | ToolLifecycleUpdate::Completed { arguments, .. }
            | ToolLifecycleUpdate::Rejected { arguments, .. } => Some(arguments),
            ToolLifecycleUpdate::Created { .. }
            | ToolLifecycleUpdate::InputStreaming { .. }
            | ToolLifecycleUpdate::Cancelled {
                arguments: None, ..
            } => None,
            ToolLifecycleUpdate::Cancelled {
                arguments: Some(arguments),
                ..
            } => Some(arguments),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context_budget() -> ContextBudgetMetadata {
        ContextBudgetMetadata {
            context_window: 200_000,
            max_input_tokens: 180_000,
            max_output_tokens: 20_000,
            compaction_ratio: 90,
            prompt_tokens: 1_000,
            tool_schema_tokens: 200,
            available_input_tokens: 179_800,
            compaction_budget: 161_820,
            usage_percent: 1,
            compaction_required: false,
        }
    }

    #[test]
    fn lifecycle_event_round_trips_with_authoritative_metadata() -> Result<(), serde_json::Error> {
        let event = ToolLifecycleEvent {
            tool_call_id: "call-1".to_owned(),
            tool_name: "overwrite_file".to_owned(),
            sequence: 4,
            emitted_at_ms: 1_234,
            run_id: Some("run-1".to_owned()),
            context_budget: Some(context_budget()),
            update: ToolLifecycleUpdate::ExecutionProgress {
                arguments: serde_json::json!({"path": "/tmp/report.md"}),
                elapsed_ms: 2_000,
                message: "wait for result [2] sec.".to_owned(),
            },
        };

        let value = serde_json::to_value(&event)?;
        assert_eq!(value["stage"], "execution_progress");
        assert_eq!(value["toolCallId"], "call-1");
        assert_eq!(value["runId"], "run-1");
        assert_eq!(value["contextBudget"]["contextWindow"], 200_000);
        assert_eq!(value["arguments"]["path"], "/tmp/report.md");
        assert_eq!(value["elapsedMs"], 2_000);

        let decoded: ToolLifecycleEvent = serde_json::from_value(value)?;
        assert_eq!(decoded, event);
        assert_eq!(decoded.stage(), ToolLifecycleStage::ExecutionProgress);
        assert!(!decoded.stage().is_terminal());
        Ok(())
    }

    #[test]
    fn active_snapshot_round_trips_streamed_input_and_progress() -> Result<(), serde_json::Error> {
        let streamed = ActiveToolInvocation {
            lifecycle: ToolLifecycleEvent {
                tool_call_id: "call-stream".to_owned(),
                tool_name: "overwrite_file".to_owned(),
                sequence: 2,
                emitted_at_ms: 2_000,
                run_id: Some("run-1".to_owned()),
                context_budget: None,
                update: ToolLifecycleUpdate::InputStreaming {
                    arguments_delta: "report".to_owned(),
                },
            },
            execution_mode: None,
            accumulated_arguments: Some(r#"{"content":"report"#.to_owned()),
            context_budget: None,
        };
        let progress = ActiveToolInvocation {
            lifecycle: ToolLifecycleEvent {
                tool_call_id: "call-progress".to_owned(),
                tool_name: "execute_command".to_owned(),
                sequence: 5,
                emitted_at_ms: 5_000,
                run_id: Some("run-1".to_owned()),
                context_budget: None,
                update: ToolLifecycleUpdate::ExecutionProgress {
                    arguments: serde_json::json!({"command": "sleep 10"}),
                    elapsed_ms: 10_000,
                    message: "wait for result [10] sec.".to_owned(),
                },
            },
            execution_mode: Some("sandbox".to_owned()),
            accumulated_arguments: None,
            context_budget: None,
        };

        let streamed_value = serde_json::to_value(&streamed)?;
        assert_eq!(
            streamed_value["accumulatedArguments"],
            r#"{"content":"report"#
        );
        assert_eq!(streamed_value["runId"], "run-1");
        let progress_value = serde_json::to_value(&progress)?;
        assert_eq!(progress_value["elapsedMs"], 10_000);
        assert_eq!(progress_value["executionMode"], "sandbox");
        assert_eq!(
            progress.arguments(),
            Some(&serde_json::json!({"command": "sleep 10"}))
        );
        Ok(())
    }
}
