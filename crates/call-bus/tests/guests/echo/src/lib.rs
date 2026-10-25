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
    marker: String,
    events: Rc<RefCell<Vec<String>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct TextParams {
    value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct TextResult {
    value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct EventsResult {
    events: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct EmptyParams {}

fn text_example() -> (Value, Value) {
    (
        serde_json::json!({"value": "ping"}),
        serde_json::json!({"value": "ping"}),
    )
}

fn events_example() -> (Value, Value) {
    (
        serde_json::json!({}),
        serde_json::json!({"events": ["ping"]}),
    )
}

fn extension() -> Value {
    serde_json::json!({
        "x-chelix-plugin": {
            "requires": { "echo_provider": ">=1.0.0" },
            "resources": [
                { "id": "notes", "kind": "dir", "access": "read", "create": true, "from": { "root": "home", "path": "notes" } },
                { "id": "scratch", "kind": "file", "access": "read_write", "create": true, "file_mode": "0600" }
            ],
            "subscribes": ["echo.happened"],
            "provides": [{ "point": "echo.ping", "order": 0, "requires": ["value"], "meta": {} }]
        },
        "x-chelix-points": [{
            "name": "ping",
            "cardinality": "many",
            "params": { "type": "object", "properties": { "value": { "type": "string" } }, "required": ["value"] },
            "result": { "type": "object" },
            "meta": { "type": "object" }
        }],
        "x-chelix-events": [{
            "name": "happened",
            "payload": { "type": "object", "properties": { "value": { "type": "string" } }, "required": ["value"] }
        }]
    })
}

plugin! {
    name: "echo",
    api: "echo",
    version: "1.0.0",
    description: "Test plugin with a method, a point, an event, and a file bind.",
    state: State,
    protocol: 1,
    extension: extension(),
    methods: [
        "ping", TextParams, TextResult, [], text_example(), ping,
        "ask", TextParams, TextResult, [], text_example(), ask,
        "emit", TextParams, TextResult, [], text_example(), emit,
        "happened", TextParams, TextResult, [], text_example(), happened,
        "events", EmptyParams, EventsResult, [], events_example(), events,
        "marker", EmptyParams, TextResult, [], events_example(), marker,
        "slow", EmptyParams, TextResult, [], events_example(), slow,
        "write_scratch", TextParams, TextResult, [], text_example(), write_scratch,
    ],
    calls: [],
    start: start,
    stop: stop,
}

async fn start(host: &Host) -> Result<State, String> {
    host.bind("scratch", "/tmp/echo-scratch").await?;
    host.call("echo_provider.pong", &serde_json::json!({"value": "from-start"}))
        .await
        .map_err(|error| error.to_string())?;
    Ok(State {
        marker: std::env::var("CHELIX_TEST_MARKER").unwrap_or_default(),
        events: Rc::new(RefCell::new(Vec::new())),
    })
}

async fn stop(_host: &Host, _state: Option<&State>) {}

async fn ping(
    _ctx: &chelix_call_bus::guest::PluginContext<State>,
    params: TextParams,
) -> Result<TextResult, chelix_call_bus::jsonrpc::RpcError> {
    Ok(TextResult { value: params.value })
}

async fn ask(
    _ctx: &chelix_call_bus::guest::PluginContext<State>,
    params: TextParams,
) -> Result<TextResult, chelix_call_bus::jsonrpc::RpcError> {
    let value = Host
        .call("echo_provider.pong", &serde_json::json!({"value": params.value}))
        .await?;
    let text = value
        .get("value")
        .and_then(|item| item.as_str())
        .ok_or_else(|| chelix_call_bus::jsonrpc::RpcError::new(-32603, "echo_provider.pong has no value"))?
        .to_owned();
    Ok(TextResult { value: text })
}

async fn happened(
    ctx: &chelix_call_bus::guest::PluginContext<State>,
    params: TextParams,
) -> Result<TextResult, chelix_call_bus::jsonrpc::RpcError> {
    ctx.state().events.borrow_mut().push(params.value.clone());
    Ok(TextResult { value: params.value })
}

async fn emit(
    _ctx: &chelix_call_bus::guest::PluginContext<State>,
    params: TextParams,
) -> Result<TextResult, chelix_call_bus::jsonrpc::RpcError> {
    Host.publish_queue("echo.happened", &serde_json::json!({"value": params.value}))
        .await
        .map_err(|error| chelix_call_bus::jsonrpc::RpcError::new(-32603, error))?;
    Ok(TextResult { value: params.value })
}

async fn events(
    ctx: &chelix_call_bus::guest::PluginContext<State>,
    _params: EmptyParams,
) -> Result<EventsResult, chelix_call_bus::jsonrpc::RpcError> {
    Ok(EventsResult {
        events: ctx.state().events.borrow().clone(),
    })
}

async fn marker(
    ctx: &chelix_call_bus::guest::PluginContext<State>,
    _params: EmptyParams,
) -> Result<TextResult, chelix_call_bus::jsonrpc::RpcError> {
    Ok(TextResult {
        value: ctx.state().marker.clone(),
    })
}

async fn slow(
    _ctx: &chelix_call_bus::guest::PluginContext<State>,
    _params: EmptyParams,
) -> Result<TextResult, chelix_call_bus::jsonrpc::RpcError> {
    Host.sleep(60_000).await;
    Ok(TextResult {
        value: "done".into(),
    })
}

async fn write_scratch(
    _ctx: &chelix_call_bus::guest::PluginContext<State>,
    params: TextParams,
) -> Result<TextResult, chelix_call_bus::jsonrpc::RpcError> {
    Host.atomic_write("scratch", "", params.value.as_bytes())
        .await
        .map_err(|error| chelix_call_bus::jsonrpc::RpcError::new(1, error))?;
    Ok(TextResult { value: params.value })
}
