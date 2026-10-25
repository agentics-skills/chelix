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
    revisions: Rc<RefCell<Vec<u64>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct ChangedParams {
    revision: u64,
    sections: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct ChangedResult {
    revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct EmptyParams {}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct ReceivedResult {
    revisions: Vec<u64>,
}

fn changed_example() -> (Value, Value) {
    (
        serde_json::json!({"revision": 2, "sections": ["tools"]}),
        serde_json::json!({"revision": 2}),
    )
}

fn received_example() -> (Value, Value) {
    (
        serde_json::json!({}),
        serde_json::json!({"revisions": [2]}),
    )
}

fn extension() -> Value {
    serde_json::json!({
        "x-chelix-plugin": {
            "requires": { "config": ">=0.1.0" },
            "resources": [],
            "subscribes": ["config.changed"],
            "provides": []
        }
    })
}

plugin! {
    name: "subscriber",
    api: "subscriber",
    version: "1.0.0",
    description: "Records config.changed revisions.",
    state: State,
    protocol: 1,
    extension: extension(),
    methods: [
        "changed", ChangedParams, ChangedResult, [], changed_example(), changed,
        "received", EmptyParams, ReceivedResult, [], received_example(), received,
    ],
    calls: [],
    start: start,
    stop: stop,
}

async fn start(_host: &Host) -> Result<State, String> {
    Ok(State {
        revisions: Rc::new(RefCell::new(Vec::new())),
    })
}

async fn stop(_host: &Host, _state: Option<&State>) {}

async fn changed(
    ctx: &chelix_call_bus::guest::PluginContext<State>,
    params: ChangedParams,
) -> Result<ChangedResult, chelix_call_bus::jsonrpc::RpcError> {
    ctx.state().revisions.borrow_mut().push(params.revision);
    Ok(ChangedResult {
        revision: params.revision,
    })
}

async fn received(
    ctx: &chelix_call_bus::guest::PluginContext<State>,
    _params: EmptyParams,
) -> Result<ReceivedResult, chelix_call_bus::jsonrpc::RpcError> {
    Ok(ReceivedResult {
        revisions: ctx.state().revisions.borrow().clone(),
    })
}
