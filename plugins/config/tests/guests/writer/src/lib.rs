#![allow(unsafe_code)]

use chelix_call_bus::guest::plugin;
use chelix_call_bus::guest::Host;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone)]
struct State;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct EmptyParams {}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct DisableResult {
    changed: bool,
    revision: u64,
}

fn example() -> (Value, Value) {
    (
        serde_json::json!({}),
        serde_json::json!({"changed": true, "revision": 2}),
    )
}

fn extension() -> Value {
    serde_json::json!({
        "x-chelix-plugin": {
            "requires": { "config": ">=0.1.0" },
            "resources": [],
            "subscribes": [],
            "provides": []
        }
    })
}

plugin! {
    name: "writer",
    api: "writer",
    version: "1.0.0",
    description: "Disables itself through config.patch.",
    state: State,
    protocol: 1,
    extension: extension(),
    methods: [
        "disable_self", EmptyParams, DisableResult, [], example(), disable_self,
    ],
    calls: [],
    start: start,
    stop: stop,
}

async fn start(_host: &Host) -> Result<State, String> {
    Ok(State)
}

async fn stop(_host: &Host, _state: Option<&State>) {}

async fn disable_self(
    _ctx: &chelix_call_bus::guest::PluginContext<State>,
    _params: EmptyParams,
) -> Result<DisableResult, chelix_call_bus::jsonrpc::RpcError> {
    let value = Host
        .call(
            "config.patch",
            &serde_json::json!({
                "ops": [{
                    "op": "set",
                    "path": "plugins.disabled",
                    "value": ["writer"]
                }]
            }),
        )
        .await?;
    Ok(DisableResult {
        changed: value
            .get("changed")
            .and_then(|item| item.as_bool())
            .ok_or_else(|| chelix_call_bus::jsonrpc::RpcError::new(-32603, "config.patch has no changed"))?,
        revision: value
            .get("revision")
            .and_then(|item| item.as_u64())
            .ok_or_else(|| chelix_call_bus::jsonrpc::RpcError::new(-32603, "config.patch has no revision"))?,
    })
}
