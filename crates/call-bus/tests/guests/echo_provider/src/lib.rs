#![allow(unsafe_code)]

use chelix_call_bus::guest::plugin;
use chelix_call_bus::guest::Host;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone)]
struct State;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct TextParams {
    value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct TextResult {
    value: String,
}

fn example() -> (Value, Value) {
    (
        serde_json::json!({"value": "ping"}),
        serde_json::json!({"value": "pong:ping"}),
    )
}

fn extension() -> Value {
    serde_json::json!({
        "x-chelix-plugin": {
            "requires": {},
            "resources": [],
            "subscribes": [],
            "provides": [{ "point": "echo.ping", "order": 1, "requires": ["value"], "meta": {} }]
        }
    })
}

plugin! {
    name: "echo_provider",
    api: "echo_provider",
    version: "1.0.0",
    description: "Second provider of echo.ping.",
    state: State,
    protocol: 1,
    extension: extension(),
    methods: [
        "pong", TextParams, TextResult, [], example(), pong,
    ],
    calls: [],
    start: start,
    stop: stop,
}

async fn start(_host: &Host) -> Result<State, String> {
    Ok(State)
}

async fn stop(_host: &Host, _state: Option<&State>) {}

async fn pong(
    _ctx: &chelix_call_bus::guest::PluginContext<State>,
    params: TextParams,
) -> Result<TextResult, chelix_call_bus::jsonrpc::RpcError> {
    Ok(TextResult {
        value: format!("pong:{}", params.value),
    })
}
