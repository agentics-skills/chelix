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
struct EmptyResult {}

fn example() -> (Value, Value) {
    (serde_json::json!({}), serde_json::json!({}))
}

fn extension() -> Value {
    serde_json::json!({
        "x-chelix-plugin": {
            "requires": {},
            "resources": [],
            "subscribes": [],
            "provides": []
        }
    })
}

plugin! {
    name: "faulty",
    api: "faulty",
    version: "1.0.0",
    description: "Test plugin that traps and returns an undeclared error code.",
    state: State,
    protocol: 1,
    extension: extension(),
    methods: [
        "trap", EmptyParams, EmptyResult, [], example(), trap,
        "undeclared", EmptyParams, EmptyResult, [(1, "declared")], example(), undeclared,
    ],
    calls: [],
    start: start,
    stop: stop,
}

async fn start(_host: &Host) -> Result<State, String> {
    Ok(State)
}

async fn stop(_host: &Host, _state: Option<&State>) {}

async fn trap(
    _ctx: &chelix_call_bus::guest::PluginContext<State>,
    _params: EmptyParams,
) -> Result<EmptyResult, chelix_call_bus::jsonrpc::RpcError> {
    panic!("faulty trap");
}

async fn undeclared(
    _ctx: &chelix_call_bus::guest::PluginContext<State>,
    _params: EmptyParams,
) -> Result<EmptyResult, chelix_call_bus::jsonrpc::RpcError> {
    Err(chelix_call_bus::jsonrpc::RpcError::new(99, "not in errors"))
}
