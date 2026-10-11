#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicUsize, Ordering};

use {super::*, crate::store_memory::InMemoryStore};

fn noop_system_event() -> SystemEventFn {
    Arc::new(|_text| {})
}

fn noop_agent_turn() -> AgentTurnFn {
    Arc::new(|_req| {
        Box::pin(async {
            Ok(AgentTurnResult {
                output: "ok".into(),
                input_tokens: None,
                output_tokens: None,
                session_key: None,
            })
        })
    })
}

fn counting_system_event(counter: Arc<AtomicUsize>) -> SystemEventFn {
    Arc::new(move |_text| {
        counter.fetch_add(1, Ordering::SeqCst);
    })
}

fn counting_agent_turn(counter: Arc<AtomicUsize>) -> AgentTurnFn {
    Arc::new(move |_req| {
        let c = Arc::clone(&counter);
        Box::pin(async move {
            c.fetch_add(1, Ordering::SeqCst);
            Ok(AgentTurnResult {
                output: "done".into(),
                input_tokens: None,
                output_tokens: None,
                session_key: None,
            })
        })
    })
}

fn make_svc(store: Arc<InMemoryStore>, sys: SystemEventFn, agent: AgentTurnFn) -> Arc<CronService> {
    CronService::new(store, sys, agent)
}

#[tokio::test]
async fn test_add_and_list() {
    let store = Arc::new(InMemoryStore::new());
    let svc = make_svc(store.clone(), noop_system_event(), noop_agent_turn());

    let job = svc
        .add(CronJobCreate {
            name: "test".into(),
            schedule: CronSchedule::Every {
                every_ms: 60_000,
                anchor_ms: None,
            },
            payload: CronPayload::AgentTurn(CronAgentTurn {
                message: "hi".into(),
                model_override: None,
                agent_id: None,
                timeout_secs: None,
                tool_choice: None,
                deliver: false,
                channel: None,
                to: None,
            }),
            session_target: SessionTarget::Isolated,
            delete_after_run: false,
            enabled: true,
            auto_prune_container: None,
        })
        .await
        .unwrap();

    let jobs = svc.list().await;
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].id, job.id);
    assert!(jobs[0].state.next_run_at_ms.is_some());
}

#[tokio::test]
async fn test_add_validates_session_target() {
    let store = Arc::new(InMemoryStore::new());
    let svc = make_svc(store, noop_system_event(), noop_agent_turn());

    // main + agentTurn should fail
    let result = svc
        .add(CronJobCreate {
            name: "bad".into(),
            schedule: CronSchedule::At {
                at_ms: 9999999999999,
            },
            payload: CronPayload::AgentTurn(CronAgentTurn {
                message: "hi".into(),
                model_override: None,
                agent_id: None,
                timeout_secs: None,
                tool_choice: None,
                deliver: false,
                channel: None,
                to: None,
            }),
            session_target: SessionTarget::Main,
            delete_after_run: false,
            enabled: true,
            auto_prune_container: None,
        })
        .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn test_update_job() {
    let store = Arc::new(InMemoryStore::new());
    let svc = make_svc(store, noop_system_event(), noop_agent_turn());

    let job = svc
        .add(CronJobCreate {
            name: "orig".into(),
            schedule: CronSchedule::Every {
                every_ms: 60_000,
                anchor_ms: None,
            },
            payload: CronPayload::AgentTurn(CronAgentTurn {
                message: "hi".into(),
                model_override: None,
                agent_id: None,
                timeout_secs: None,
                tool_choice: None,
                deliver: false,
                channel: None,
                to: None,
            }),
            session_target: SessionTarget::Isolated,
            delete_after_run: false,
            enabled: true,
            auto_prune_container: None,
        })
        .await
        .unwrap();

    let updated = svc
        .update(&job.id, CronJobPatch {
            name: Some("renamed".into()),
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(updated.name, "renamed");
}

#[tokio::test]
async fn test_remove_job() {
    let store = Arc::new(InMemoryStore::new());
    let svc = make_svc(store, noop_system_event(), noop_agent_turn());

    let job = svc
        .add(CronJobCreate {
            name: "del".into(),
            schedule: CronSchedule::Every {
                every_ms: 60_000,
                anchor_ms: None,
            },
            payload: CronPayload::AgentTurn(CronAgentTurn {
                message: "hi".into(),
                model_override: None,
                agent_id: None,
                timeout_secs: None,
                tool_choice: None,
                deliver: false,
                channel: None,
                to: None,
            }),
            session_target: SessionTarget::Isolated,
            delete_after_run: false,
            enabled: true,
            auto_prune_container: None,
        })
        .await
        .unwrap();

    svc.remove(&job.id).await.unwrap();
    assert!(svc.list().await.is_empty());
}

#[tokio::test]
async fn test_status() {
    let store = Arc::new(InMemoryStore::new());
    let svc = make_svc(store, noop_system_event(), noop_agent_turn());

    let status = svc.status().await;
    assert!(!status.running);
    assert_eq!(status.job_count, 0);
}

#[tokio::test]
async fn test_force_run() {
    let counter = Arc::new(AtomicUsize::new(0));
    let store = Arc::new(InMemoryStore::new());
    let svc = make_svc(
        store,
        noop_system_event(),
        counting_agent_turn(counter.clone()),
    );

    let job = svc
        .add(CronJobCreate {
            name: "force".into(),
            schedule: CronSchedule::Every {
                every_ms: 999_999_999,
                anchor_ms: None,
            },
            payload: CronPayload::AgentTurn(CronAgentTurn {
                message: "go".into(),
                model_override: None,
                agent_id: None,
                timeout_secs: None,
                tool_choice: None,
                deliver: false,
                channel: None,
                to: None,
            }),
            session_target: SessionTarget::Isolated,
            delete_after_run: false,
            enabled: true,
            auto_prune_container: None,
        })
        .await
        .unwrap();

    svc.run(&job.id, false).await.unwrap();
    // Give the spawned task a moment.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_run_disabled_fails_without_force() {
    let store = Arc::new(InMemoryStore::new());
    let svc = make_svc(store, noop_system_event(), noop_agent_turn());

    let job = svc
        .add(CronJobCreate {
            name: "disabled".into(),
            schedule: CronSchedule::Every {
                every_ms: 60_000,
                anchor_ms: None,
            },
            payload: CronPayload::AgentTurn(CronAgentTurn {
                message: "hi".into(),
                model_override: None,
                agent_id: None,
                timeout_secs: None,
                tool_choice: None,
                deliver: false,
                channel: None,
                to: None,
            }),
            session_target: SessionTarget::Isolated,
            delete_after_run: false,
            enabled: false,
            auto_prune_container: None,
        })
        .await
        .unwrap();

    assert!(svc.run(&job.id, false).await.is_err());
    assert!(svc.run(&job.id, true).await.is_ok());
}

#[tokio::test]
async fn test_system_event_execution() {
    let counter = Arc::new(AtomicUsize::new(0));
    let store = Arc::new(InMemoryStore::new());
    let svc = make_svc(
        store,
        counting_system_event(counter.clone()),
        noop_agent_turn(),
    );

    let job = svc
        .add(CronJobCreate {
            name: "sys".into(),
            schedule: CronSchedule::Every {
                every_ms: 60_000,
                anchor_ms: None,
            },
            payload: CronPayload::SystemEvent(CronSystemEvent {
                text: "ping".into(),
            }),
            session_target: SessionTarget::Main,
            delete_after_run: false,
            enabled: true,
            auto_prune_container: None,
        })
        .await
        .unwrap();

    svc.run(&job.id, true).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_start_stop() {
    let store = Arc::new(InMemoryStore::new());
    let svc = make_svc(store, noop_system_event(), noop_agent_turn());

    svc.start().await.unwrap();
    let status = svc.status().await;
    assert!(status.running);

    svc.stop().await;
    let status = svc.status().await;
    assert!(!status.running);
}

#[tokio::test]
async fn test_one_shot_disabled_after_run() {
    let store = Arc::new(InMemoryStore::new());
    let svc = make_svc(store, noop_system_event(), noop_agent_turn());

    // Use a past at_ms so compute_next_run returns None after execution.
    let job = svc
        .add(CronJobCreate {
            name: "oneshot".into(),
            schedule: CronSchedule::At { at_ms: 1000 }, // far past
            payload: CronPayload::AgentTurn(CronAgentTurn {
                message: "once".into(),
                model_override: None,
                agent_id: None,
                timeout_secs: None,
                tool_choice: None,
                deliver: false,
                channel: None,
                to: None,
            }),
            session_target: SessionTarget::Isolated,
            delete_after_run: false,
            enabled: true,
            auto_prune_container: None,
        })
        .await
        .unwrap();

    // next_run_at_ms is None because at_ms is in the past, but job is still enabled.
    svc.run(&job.id, true).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let jobs = svc.list().await;
    let j = jobs.iter().find(|j| j.id == job.id).unwrap();
    assert!(!j.enabled, "one-shot job should be disabled after run");
}

#[tokio::test]
async fn test_rate_limiting() {
    let store = Arc::new(InMemoryStore::new());
    // Create service with strict rate limit: 3 jobs per 60 seconds.
    let svc = CronService::with_config(
        store,
        noop_system_event(),
        noop_agent_turn(),
        None,
        RateLimitConfig {
            max_per_window: 3,
            window_ms: 60_000,
        },
    );

    let create_job = || CronJobCreate {
        name: "test".into(),
        schedule: CronSchedule::Every {
            every_ms: 60_000,
            anchor_ms: None,
        },
        payload: CronPayload::AgentTurn(CronAgentTurn {
            message: "hi".into(),
            model_override: None,
            agent_id: None,
            timeout_secs: None,
            tool_choice: None,
            deliver: false,
            channel: None,
            to: None,
        }),
        session_target: SessionTarget::Isolated,
        delete_after_run: false,
        enabled: true,
        auto_prune_container: None,
    };

    // First 3 jobs should succeed.
    svc.add(create_job()).await.unwrap();
    svc.add(create_job()).await.unwrap();
    svc.add(create_job()).await.unwrap();

    // 4th job should fail due to rate limit.
    let result = svc.add(create_job()).await;
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("rate limit exceeded")
    );
}

#[tokio::test]
async fn test_start_executes_due_jobs_and_records_runs() {
    let counter = Arc::new(AtomicUsize::new(0));
    let store = Arc::new(InMemoryStore::new());
    let svc = make_svc(
        store,
        noop_system_event(),
        counting_agent_turn(Arc::clone(&counter)),
    );

    let job = svc
        .add(CronJobCreate {
            name: "live-timer".into(),
            schedule: CronSchedule::Every {
                every_ms: 25,
                anchor_ms: None,
            },
            payload: CronPayload::AgentTurn(CronAgentTurn {
                message: "tick".into(),
                model_override: None,
                agent_id: None,
                timeout_secs: None,
                tool_choice: None,
                deliver: false,
                channel: None,
                to: None,
            }),
            session_target: SessionTarget::Isolated,
            delete_after_run: false,
            enabled: true,
            auto_prune_container: None,
        })
        .await
        .unwrap();

    svc.start().await.unwrap();

    tokio::time::timeout(Duration::from_secs(2), async {
        while counter.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("cron scheduler did not execute any due jobs in time");

    let runs = svc.runs(&job.id, 10).await.unwrap();
    assert!(
        !runs.is_empty(),
        "expected at least one persisted run record"
    );

    svc.stop().await;
}

#[tokio::test]
async fn test_clear_stuck_jobs_handles_future_running_at_without_overflow() {
    let store = Arc::new(InMemoryStore::new());
    let svc = make_svc(store, noop_system_event(), noop_agent_turn());

    let job = svc
        .add(CronJobCreate {
            name: "future-running-at".into(),
            schedule: CronSchedule::Every {
                every_ms: 60_000,
                anchor_ms: None,
            },
            payload: CronPayload::AgentTurn(CronAgentTurn {
                message: "hi".into(),
                model_override: None,
                agent_id: None,
                timeout_secs: None,
                tool_choice: None,
                deliver: false,
                channel: None,
                to: None,
            }),
            session_target: SessionTarget::Isolated,
            delete_after_run: false,
            enabled: true,
            auto_prune_container: None,
        })
        .await
        .unwrap();

    let now = now_ms();
    svc.update_job_state(&job.id, |state| {
        state.running_at_ms = Some(now + 1_000);
    })
    .await;

    svc.clear_stuck_jobs(now).await;

    let jobs = svc.list().await;
    let job_state = jobs
        .iter()
        .find(|j| j.id == job.id)
        .expect("job should exist");
    assert_eq!(job_state.state.running_at_ms, Some(now + 1_000));
    assert!(job_state.state.last_error.is_none());
}

#[tokio::test]
async fn test_deliver_requires_channel_and_to() {
    let store = Arc::new(InMemoryStore::new());
    let svc = make_svc(store, noop_system_event(), noop_agent_turn());

    // deliver=true but no channel/to → error
    let err = svc
        .add(CronJobCreate {
            name: "bad".into(),
            schedule: CronSchedule::Every {
                every_ms: 60_000,
                anchor_ms: None,
            },
            payload: CronPayload::AgentTurn(CronAgentTurn {
                message: "hi".into(),
                model_override: None,
                agent_id: None,
                timeout_secs: None,
                tool_choice: None,
                deliver: true,
                channel: None,
                to: None,
            }),
            session_target: SessionTarget::Isolated,
            delete_after_run: false,
            enabled: true,
            auto_prune_container: None,
        })
        .await;
    assert!(err.is_err());
    assert!(
        err.unwrap_err()
            .to_string()
            .contains("deliver=true requires")
    );
}

#[tokio::test]
async fn test_deliver_with_both_fields_succeeds() {
    let store = Arc::new(InMemoryStore::new());
    let svc = make_svc(store, noop_system_event(), noop_agent_turn());

    let result = svc
        .add(CronJobCreate {
            name: "good".into(),
            schedule: CronSchedule::Every {
                every_ms: 60_000,
                anchor_ms: None,
            },
            payload: CronPayload::AgentTurn(CronAgentTurn {
                message: "hi".into(),
                model_override: None,
                agent_id: None,
                timeout_secs: None,
                tool_choice: None,
                deliver: true,
                channel: Some("telegram_bot".into()),
                to: Some("123456".into()),
            }),
            session_target: SessionTarget::Isolated,
            delete_after_run: false,
            enabled: true,
            auto_prune_container: None,
        })
        .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_deliver_false_allows_missing_channel() {
    let store = Arc::new(InMemoryStore::new());
    let svc = make_svc(store, noop_system_event(), noop_agent_turn());

    let result = svc
        .add(CronJobCreate {
            name: "ok".into(),
            schedule: CronSchedule::Every {
                every_ms: 60_000,
                anchor_ms: None,
            },
            payload: CronPayload::AgentTurn(CronAgentTurn {
                message: "hi".into(),
                model_override: None,
                agent_id: None,
                timeout_secs: None,
                tool_choice: None,
                deliver: false,
                channel: None,
                to: None,
            }),
            session_target: SessionTarget::Isolated,
            delete_after_run: false,
            enabled: true,
            auto_prune_container: None,
        })
        .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_deliver_empty_string_channel_fails() {
    let store = Arc::new(InMemoryStore::new());
    let svc = make_svc(store, noop_system_event(), noop_agent_turn());

    let err = svc
        .add(CronJobCreate {
            name: "empty".into(),
            schedule: CronSchedule::Every {
                every_ms: 60_000,
                anchor_ms: None,
            },
            payload: CronPayload::AgentTurn(CronAgentTurn {
                message: "hi".into(),
                model_override: None,
                agent_id: None,
                timeout_secs: None,
                tool_choice: None,
                deliver: true,
                channel: Some(String::new()),
                to: Some("123".into()),
            }),
            session_target: SessionTarget::Isolated,
            delete_after_run: false,
            enabled: true,
            auto_prune_container: None,
        })
        .await;
    assert!(err.is_err());
    assert!(
        err.unwrap_err()
            .to_string()
            .contains("deliver=true requires")
    );
}
