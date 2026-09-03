#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use chelix_cron::{
    service::{AgentTurnFn, CronService, SystemEventFn},
    store_memory::InMemoryStore,
};

use super::*;

fn noop_sys() -> SystemEventFn {
    Arc::new(|_| {})
}

fn noop_agent() -> AgentTurnFn {
    Arc::new(|_| {
        Box::pin(async {
            Ok(chelix_cron::service::AgentTurnResult {
                output: "ok".into(),
                input_tokens: None,
                output_tokens: None,
                session_key: None,
            })
        })
    })
}

fn make_tool() -> CronTool {
    let store = Arc::new(InMemoryStore::new());
    let svc = CronService::new(store, noop_sys(), noop_agent());
    CronTool::new(svc)
}

#[tokio::test]
async fn test_status() {
    let tool = make_tool();
    let result = tool.execute(json!({ "action": "status" })).await.unwrap();
    assert_eq!(result["running"], false);
}

#[tokio::test]
async fn test_list_empty() {
    let tool = make_tool();
    let result = tool.execute(json!({ "action": "list" })).await.unwrap();
    assert!(result.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn test_add_and_list() {
    let tool = make_tool();
    let add_result = tool
        .execute(json!({
            "action": "add",
            "job": {
                "name": "test job",
                "schedule": { "kind": "every", "every_ms": 60000 },
                "payload": { "kind": "agentTurn", "message": "do stuff" },
                "sessionTarget": "isolated"
            }
        }))
        .await
        .unwrap();

    assert!(add_result.get("id").is_some());

    let list = tool.execute(json!({ "action": "list" })).await.unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn test_remove() {
    let tool = make_tool();
    let add = tool
        .execute(json!({
            "action": "add",
            "job": {
                "name": "to remove",
                "schedule": { "kind": "every", "every_ms": 60000 },
                "payload": { "kind": "agentTurn", "message": "x" },
                "sessionTarget": "isolated"
            }
        }))
        .await
        .unwrap();

    let id = add["id"].as_str().unwrap();
    let result = tool
        .execute(json!({ "action": "remove", "id": id }))
        .await
        .unwrap();
    assert_eq!(result["removed"].as_str().unwrap(), id);
}

#[tokio::test]
async fn test_unknown_action() {
    let tool = make_tool();
    let result = tool.execute(json!({ "action": "nope" })).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_runs_empty() {
    let tool = make_tool();
    let result = tool
        .execute(json!({ "action": "runs", "id": "nonexistent" }))
        .await
        .unwrap();
    assert!(result.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn test_add_accepts_cron_expression_string_schedule() {
    let tool = make_tool();
    let add_result = tool
        .execute(json!({
            "action": "add",
            "job": {
                "name": "news update",
                "schedule": "5 11 * * *",
                "payload": { "kind": "agentTurn", "message": "fetch weather and summarize" },
                "sessionTarget": "isolated"
            }
        }))
        .await
        .unwrap();

    assert_eq!(add_result["schedule"]["kind"], "cron");
    assert_eq!(add_result["schedule"]["expr"], "5 11 * * *");
}

#[tokio::test]
async fn test_add_infers_schedule_kind_from_expr_without_kind() {
    let tool = make_tool();
    let add_result = tool
        .execute(json!({
            "action": "add",
            "job": {
                "name": "daily digest",
                "schedule": { "expr": "0 9 * * *" },
                "payload": { "kind": "agentTurn", "message": "send daily digest" },
                "sessionTarget": "isolated"
            }
        }))
        .await
        .unwrap();

    assert_eq!(add_result["schedule"]["kind"], "cron");
    assert_eq!(add_result["schedule"]["expr"], "0 9 * * *");
}

#[tokio::test]
async fn test_add_accepts_canonical_agent_turn_execution_fields() {
    let tool = make_tool();
    let add_result = tool
        .execute(json!({
            "action": "add",
            "job": {
                "name": "configured turn",
                "schedule": { "kind": "every", "every_ms": 60000 },
                "payload": {
                    "kind": "agentTurn",
                    "message": "run diagnostics",
                    "modelOverride": {
                        "model": "test::model",
                        "reasoningEffort": "low"
                    },
                    "agentId": "main",
                    "timeoutSecs": "30s",
                    "toolChoice": {
                        "type": "tool",
                        "name": "overwrite_file"
                    }
                },
                "sessionTarget": "isolated"
            }
        }))
        .await
        .unwrap();

    assert_eq!(
        add_result["payload"]["modelOverride"]["model"],
        "test::model"
    );
    assert_eq!(
        add_result["payload"]["modelOverride"]["reasoningEffort"],
        "low"
    );
    assert_eq!(add_result["payload"]["agentId"], "main");
    assert_eq!(add_result["payload"]["timeoutSecs"], 30);
    assert_eq!(add_result["payload"]["toolChoice"]["type"], "tool");
    assert_eq!(
        add_result["payload"]["toolChoice"]["name"],
        "overwrite_file"
    );
}

#[tokio::test]
async fn test_update_accepts_schedule_string_patch() {
    let tool = make_tool();
    let add_result = tool
        .execute(json!({
            "action": "add",
            "job": {
                "name": "to patch",
                "schedule": { "kind": "every", "every_ms": 300000 },
                "payload": { "kind": "agentTurn", "message": "run task" },
                "sessionTarget": "isolated"
            }
        }))
        .await
        .unwrap();
    let id = add_result["id"].as_str().unwrap();

    let updated = tool
        .execute(json!({
            "action": "update",
            "id": id,
            "patch": { "schedule": "*/15 * * * *" }
        }))
        .await
        .unwrap();

    assert_eq!(updated["schedule"]["kind"], "cron");
    assert_eq!(updated["schedule"]["expr"], "*/15 * * * *");
}

#[tokio::test]
async fn test_add_accepts_schedule_alias_fields_and_payload_duration() {
    let tool = make_tool();
    let add_result = tool
        .execute(json!({
            "action": "add",
            "job": {
                "name": "alias fields",
                "session_target": "isolated",
                "schedule": { "kind": "interval", "everyMs": "5m" },
                "payload": { "kind": "agentTurn", "message": "do work", "timeoutSecs": "30s" }
            }
        }))
        .await
        .unwrap();

    assert_eq!(add_result["sessionTarget"], "isolated");
    assert_eq!(add_result["schedule"]["kind"], "every");
    assert_eq!(add_result["schedule"]["every_ms"], 300000);
    assert_eq!(add_result["payload"]["kind"], "agentTurn");
    assert_eq!(add_result["payload"]["message"], "do work");
    assert_eq!(add_result["payload"]["timeoutSecs"], 30);
}

#[tokio::test]
async fn test_add_rejects_execution_override() {
    let tool = make_tool();
    let result = tool
        .execute(json!({
            "action": "add",
            "job": {
                "name": "forbidden execution override",
                "schedule": { "kind": "every", "every_ms": 60000 },
                "payload": { "kind": "agentTurn", "message": "run diagnostics" },
                "execution": {
                    "target": "sandbox",
                    "image": "ubuntu:26.04"
                }
            }
        }))
        .await;

    let error = result.unwrap_err().to_string();
    assert!(
        error.contains("unknown field `execution`"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn test_add_accepts_delivery_fields_for_agent_turn() {
    let tool = make_tool();
    let add_result = tool
        .execute(json!({
            "action": "add",
            "job": {
                "name": "delivered run",
                "schedule": { "kind": "every", "every_ms": 60000 },
                "payload": {
                    "kind": "agentTurn",
                    "message": "post an update",
                    "deliver": true,
                    "channel": "bot-main",
                    "to": "123456"
                },
                "sessionTarget": "isolated"
            }
        }))
        .await
        .unwrap();

    assert_eq!(add_result["payload"]["kind"], "agentTurn");
    assert_eq!(add_result["payload"]["deliver"], true);
    assert_eq!(add_result["payload"]["channel"], "bot-main");
    assert_eq!(add_result["payload"]["to"], "123456");
}

#[tokio::test]
async fn test_update_rejects_sandbox_override() {
    let tool = make_tool();
    let add_result = tool
        .execute(json!({
            "action": "add",
            "job": {
                "name": "switch execution",
                "schedule": { "kind": "every", "every_ms": 60000 },
                "payload": { "kind": "agentTurn", "message": "run task" },
                "sessionTarget": "isolated"
            }
        }))
        .await
        .unwrap();
    let id = add_result["id"].as_str().unwrap();

    let result = tool
        .execute(json!({
            "action": "update",
            "id": id,
            "patch": { "sandbox": { "enabled": false } }
        }))
        .await;

    let error = result.unwrap_err().to_string();
    assert!(
        error.contains("unknown field `sandbox`"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn test_update_accepts_delivery_fields_in_patch() {
    let tool = make_tool();
    let add_result = tool
        .execute(json!({
            "action": "add",
            "job": {
                "name": "toggle delivery",
                "schedule": { "kind": "every", "every_ms": 60000 },
                "payload": { "kind": "agentTurn", "message": "run task" },
                "sessionTarget": "isolated"
            }
        }))
        .await
        .unwrap();
    let id = add_result["id"].as_str().unwrap();

    let updated = tool
        .execute(json!({
            "action": "update",
            "id": id,
            "patch": {
                "payload": {
                    "kind": "agentTurn",
                    "message": "run task",
                    "deliver": true,
                    "channel": "bot-main",
                    "to": "123456"
                }
            }
        }))
        .await
        .unwrap();

    assert_eq!(updated["payload"]["deliver"], true);
    assert_eq!(updated["payload"]["channel"], "bot-main");
    assert_eq!(updated["payload"]["to"], "123456");
}

#[test]
fn test_parameters_schema_uses_closed_canonical_payload_vocabulary() {
    fn contains_composite_keyword(value: &Value) -> bool {
        match value {
            Value::Object(obj) => {
                if ["oneOf", "anyOf", "allOf"]
                    .iter()
                    .any(|key| obj.contains_key(*key))
                {
                    return true;
                }
                obj.values().any(contains_composite_keyword)
            },
            Value::Array(items) => items.iter().any(contains_composite_keyword),
            _ => false,
        }
    }

    let tool = make_tool();
    let schema = tool.parameters_schema();
    assert!(
        !contains_composite_keyword(&schema),
        "cron tool schema must avoid composite keywords for OpenAI Responses API compatibility"
    );
    let job_properties = &schema["properties"]["job"]["properties"];
    assert!(job_properties.get("sandbox").is_none());
    assert!(job_properties.get("execution").is_none());

    let payload = &job_properties["payload"];
    assert_eq!(payload["type"], "object");
    assert_eq!(payload["additionalProperties"], false);
    let payload_properties = &payload["properties"];
    for field in [
        "kind",
        "text",
        "message",
        "modelOverride",
        "agentId",
        "timeoutSecs",
        "toolChoice",
        "deliver",
        "channel",
        "to",
    ] {
        assert!(payload_properties.get(field).is_some(), "missing {field}");
    }
    assert_eq!(
        payload_properties["modelOverride"]["additionalProperties"],
        false
    );
    assert_eq!(
        payload_properties["toolChoice"]["additionalProperties"],
        false
    );
}

#[tokio::test]
async fn test_add_rejects_partial_model_override() {
    let tool = make_tool();
    let result = tool
        .execute(json!({
            "action": "add",
            "job": {
                "name": "partial override",
                "schedule": { "kind": "every", "every_ms": 60000 },
                "payload": {
                    "kind": "agentTurn",
                    "message": "run diagnostics",
                    "modelOverride": { "model": "test::model" }
                },
                "sessionTarget": "isolated"
            }
        }))
        .await;

    let error = result.unwrap_err().to_string();
    assert!(
        error.contains("missing field `reasoningEffort`"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn test_add_rejects_ambiguous_schedule_without_kind() {
    let tool = make_tool();
    let result = tool
        .execute(json!({
            "action": "add",
            "job": {
                "name": "ambiguous",
                "schedule": {
                    "expr": "*/5 * * * *",
                    "every_ms": 60000
                },
                "payload": { "kind": "agentTurn", "message": "x" },
                "sessionTarget": "isolated"
            }
        }))
        .await;

    let err = result.unwrap_err().to_string();
    assert!(err.contains("ambiguous fields"), "unexpected error: {err}");
}

#[test]
fn test_normalize_wake_mode_aliases() {
    assert_eq!(normalize_wake_mode("now"), Some("now"));
    assert_eq!(normalize_wake_mode("immediate"), Some("now"));
    assert_eq!(normalize_wake_mode("immediately"), Some("now"));
    assert_eq!(normalize_wake_mode("NOW"), Some("now"));
    assert_eq!(normalize_wake_mode("nextHeartbeat"), Some("nextHeartbeat"));
    assert_eq!(normalize_wake_mode("next_heartbeat"), Some("nextHeartbeat"));
    assert_eq!(normalize_wake_mode("next-heartbeat"), Some("nextHeartbeat"));
    assert_eq!(normalize_wake_mode("next"), Some("nextHeartbeat"));
    assert_eq!(normalize_wake_mode("default"), Some("nextHeartbeat"));
    assert_eq!(normalize_wake_mode("bogus"), None);
}

#[tokio::test]
async fn test_add_with_wake_mode() {
    let tool = make_tool();
    let result = tool
        .execute(json!({
            "action": "add",
            "job": {
                "name": "wake test",
                "schedule": { "kind": "every", "every_ms": 60000 },
                "payload": { "kind": "agentTurn", "message": "go" },
                "wakeMode": "now"
            }
        }))
        .await
        .unwrap();
    assert_eq!(result["wakeMode"], "now");
}

#[tokio::test]
async fn test_add_with_wake_mode_alias() {
    let tool = make_tool();
    let result = tool
        .execute(json!({
            "action": "add",
            "job": {
                "name": "alias wake",
                "schedule": { "kind": "every", "every_ms": 60000 },
                "payload": { "kind": "agentTurn", "message": "go" },
                "wake_mode": "immediate"
            }
        }))
        .await
        .unwrap();
    assert_eq!(result["wakeMode"], "now");
}

#[tokio::test]
async fn test_update_wake_mode() {
    let tool = make_tool();
    let add = tool
        .execute(json!({
            "action": "add",
            "job": {
                "name": "update wake",
                "schedule": { "kind": "every", "every_ms": 60000 },
                "payload": { "kind": "agentTurn", "message": "go" }
            }
        }))
        .await
        .unwrap();
    let id = add["id"].as_str().unwrap();

    let updated = tool
        .execute(json!({
            "action": "update",
            "id": id,
            "patch": { "wakeMode": "now" }
        }))
        .await
        .unwrap();
    assert_eq!(updated["wakeMode"], "now");
}

// --- Stringified-JSON rescue tests (issue #430) ---

#[tokio::test]
async fn test_add_accepts_stringified_job() {
    let tool = make_tool();
    let job_json = serde_json::to_string(&json!({
        "name": "stringified job",
        "schedule": { "kind": "every", "every_ms": 60000 },
        "payload": { "kind": "agentTurn", "message": "do stuff" },
        "sessionTarget": "isolated"
    }))
    .unwrap();

    let result = tool
        .execute(json!({ "action": "add", "job": job_json }))
        .await
        .unwrap();

    assert!(result.get("id").is_some());
    assert_eq!(result["name"], "stringified job");
}

#[tokio::test]
async fn test_add_accepts_flat_params_without_job_wrapper() {
    let tool = make_tool();
    let result = tool
        .execute(json!({
            "action": "add",
            "name": "flat params",
            "schedule": { "kind": "every", "every_ms": 60000 },
            "payload": { "kind": "agentTurn", "message": "run" },
            "sessionTarget": "isolated"
        }))
        .await
        .unwrap();

    assert!(result.get("id").is_some());
    assert_eq!(result["name"], "flat params");
}

#[tokio::test]
async fn test_add_accepts_stringified_schedule() {
    let tool = make_tool();
    let result = tool
        .execute(json!({
            "action": "add",
            "job": {
                "name": "stringified nested",
                "schedule": r#"{"kind":"cron","expr":"0 9 * * 1"}"#,
                "payload": { "kind": "agentTurn", "message": "hello" },
                "sessionTarget": "isolated"
            }
        }))
        .await
        .unwrap();

    assert!(result.get("id").is_some());
    assert_eq!(result["schedule"]["kind"], "cron");
    assert_eq!(result["schedule"]["expr"], "0 9 * * 1");
    assert_eq!(result["payload"]["kind"], "agentTurn");
    assert_eq!(result["payload"]["message"], "hello");
}

#[tokio::test]
async fn test_update_accepts_stringified_patch() {
    let tool = make_tool();
    let add = tool
        .execute(json!({
            "action": "add",
            "job": {
                "name": "to patch",
                "schedule": { "kind": "every", "every_ms": 60000 },
                "payload": { "kind": "agentTurn", "message": "x" },
                "sessionTarget": "isolated"
            }
        }))
        .await
        .unwrap();
    let id = add["id"].as_str().unwrap();

    let patch_json = serde_json::to_string(&json!({ "name": "patched" })).unwrap();
    let updated = tool
        .execute(json!({ "action": "update", "id": id, "patch": patch_json }))
        .await
        .unwrap();

    assert_eq!(updated["name"], "patched");
}

#[tokio::test]
async fn test_stringified_job_with_invalid_json_is_rejected() {
    let tool = make_tool();
    let result = tool
        .execute(json!({ "action": "add", "job": "not valid json {" }))
        .await;
    assert!(result.is_err());
}
