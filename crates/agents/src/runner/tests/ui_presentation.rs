use {
    crate::{
        runner::{OnToolLifecycle, RunnerToolLifecycleEvent, deliver_tool_lifecycle},
        tool_registry::{AgentTool, ToolRegistry},
    },
    async_trait::async_trait,
    chelix_common::tool_lifecycle::{ToolLifecycleEvent, ToolLifecycleUpdate},
    chelix_sessions::ui_history_types::{UiPresentation, UiPresentationDocument},
    serde_json::{Value, json},
    std::sync::{Arc, Mutex},
};

struct PresentedTool {
    fail: bool,
}

#[async_trait]
impl AgentTool for PresentedTool {
    fn name(&self) -> &str {
        "presented_tool"
    }

    fn description(&self) -> &str {
        "Lifecycle presentation fixture"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn ui_presentation(
        &self,
        event: &ToolLifecycleEvent,
    ) -> anyhow::Result<Option<UiPresentation>> {
        anyhow::ensure!(!self.fail, "presentation failed");
        Ok(Some(UiPresentation {
            document: Some(UiPresentationDocument::Text(format!("{:?}", event.stage()))),
            ..UiPresentation::default()
        }))
    }

    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        Ok(params)
    }
}

fn event(update: ToolLifecycleUpdate) -> ToolLifecycleEvent {
    ToolLifecycleEvent {
        tool_call_id: "call-1".into(),
        tool_name: "presented_tool".into(),
        sequence: 1,
        emitted_at_ms: 42,
        run_id: Some("run-1".into()),
        context_budget: None,
        update,
    }
}

#[tokio::test]
async fn lifecycle_presentation_preserves_each_stage_and_raw_result() {
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(PresentedTool { fail: false }));
    let received = Arc::new(Mutex::new(Vec::new()));
    let output = Arc::clone(&received);
    let callback: OnToolLifecycle = Arc::new(move |event| {
        let output = Arc::clone(&output);
        Box::pin(async move {
            output.lock().unwrap().push(event);
            Ok(())
        })
    });
    let arguments = json!({"path": "file.txt"});
    let stages = [
        ToolLifecycleUpdate::Created {
            provider_index: Some(0),
        },
        ToolLifecycleUpdate::InputStreaming {
            arguments_delta: "{\"path\":".into(),
        },
        ToolLifecycleUpdate::InputReady {
            arguments: arguments.clone(),
        },
        ToolLifecycleUpdate::WaitingForExecution {
            arguments: arguments.clone(),
        },
        ToolLifecycleUpdate::Executing {
            arguments: arguments.clone(),
            started_at_ms: 42,
        },
        ToolLifecycleUpdate::ExecutionProgress {
            arguments: arguments.clone(),
            elapsed_ms: 1,
            message: "working".into(),
        },
        ToolLifecycleUpdate::ResultReady {
            arguments: arguments.clone(),
            success: true,
            result: Some("done".into()),
            error: None,
        },
        ToolLifecycleUpdate::Completed {
            arguments: arguments.clone(),
            success: true,
            result: Some("done".into()),
            error: None,
        },
        ToolLifecycleUpdate::Rejected {
            arguments: arguments.clone(),
            reason: "denied".into(),
            result: "denied".into(),
        },
        ToolLifecycleUpdate::Cancelled {
            arguments: Some(arguments),
            reason: "stopped".into(),
        },
    ];
    for stage in stages {
        let lifecycle = event(stage);
        let mut runner_event = RunnerToolLifecycleEvent::new(lifecycle.clone());
        runner_event.raw_result = Some(json!({"original": true}));
        deliver_tool_lifecycle(&tools, Some(&callback), runner_event)
            .await
            .unwrap();
        let received = received.lock().unwrap();
        let delivered = received.last().unwrap();
        assert_eq!(delivered.lifecycle, lifecycle);
        assert_eq!(delivered.raw_result, Some(json!({"original": true})));
        assert_eq!(
            serde_json::to_value(&delivered.ui_presentation.as_ref().unwrap().document).unwrap(),
            json!({"format": "text", "content": format!("{:?}", lifecycle.stage())})
        );
    }
}

#[tokio::test]
async fn presentation_failure_refuses_lifecycle_delivery() {
    let mut tools = ToolRegistry::new();
    tools.register(Box::new(PresentedTool { fail: true }));
    let callback: OnToolLifecycle =
        Arc::new(|_| Box::pin(async { panic!("failed presentation must not be delivered") }));
    let result = deliver_tool_lifecycle(
        &tools,
        Some(&callback),
        RunnerToolLifecycleEvent::new(event(ToolLifecycleUpdate::Created {
            provider_index: Some(0),
        })),
    )
    .await;
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("presentation failed")
    );
}
