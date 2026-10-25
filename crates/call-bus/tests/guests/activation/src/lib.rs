#![allow(unsafe_code)]

use std::cell::RefCell;
use std::rc::Rc;

use chelix_call_bus::guest::plugin;
use chelix_call_bus::guest::Host;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone)]
struct State {
    disabled: Rc<RefCell<Vec<String>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct SetDisabledParams {
    disabled: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct SetDisabledResult {
    disabled: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct ActivationParams {}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct ActivationResult {
    disabled: Vec<String>,
}

fn set_example() -> (Value, Value) {
    (
        serde_json::json!({"disabled": ["echo"]}),
        serde_json::json!({"disabled": ["echo"]}),
    )
}

fn extension() -> Value {
    serde_json::json!({
        "x-chelix-plugin": {
            "requires": {},
            "resources": [],
            "subscribes": [],
            "provides": [{ "point": "bus.activation", "order": 0, "requires": [], "meta": {} }]
        },
        "x-chelix-points": [],
        "x-chelix-events": []
    })
}

plugin! {
    name: "activation",
    api: "activation",
    version: "1.0.0",
    description: "Test provider of bus.activation.",
    state: State,
    protocol: 1,
    extension: extension(),
    methods: [
        "set_disabled", SetDisabledParams, SetDisabledResult, [], set_example(), set_disabled,
    ],
    calls: [
        "bus.activation", ActivationParams, ActivationResult, activation,
    ],
    start: start,
    stop: stop,
}

async fn start(_host: &Host) -> Result<State, String> {
    Ok(State {
        disabled: Rc::new(RefCell::new(Vec::new())),
    })
}

async fn stop(_host: &Host, _state: Option<&State>) {}

async fn set_disabled(
    ctx: &chelix_call_bus::guest::PluginContext<State>,
    params: SetDisabledParams,
) -> Result<SetDisabledResult, chelix_call_bus::jsonrpc::RpcError> {
    *ctx.state().disabled.borrow_mut() = params.disabled.clone();
    let host = Host;
    host.call("bus.plugins.reload", &serde_json::json!({}))
        .await
        .map_err(|error| error)?;
    Ok(SetDisabledResult {
        disabled: params.disabled,
    })
}

async fn activation(
    ctx: &chelix_call_bus::guest::PluginContext<State>,
    _params: ActivationParams,
) -> Result<ActivationResult, chelix_call_bus::jsonrpc::RpcError> {
    Ok(ActivationResult {
        disabled: ctx.state().disabled.borrow().clone(),
    })
}
