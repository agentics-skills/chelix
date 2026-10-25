use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::Value;

use crate::bus::{BusOptions, CallBus};
use crate::info::PluginStatus;
use crate::testing::exe_dir;

fn env_with_marker() -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert("CHELIX_TEST_MARKER".to_owned(), "marker".to_owned());
    env.insert("HOME".to_owned(), "/tmp".to_owned());
    env
}

async fn bus() -> (tempfile::TempDir, std::sync::Arc<CallBus>) {
    let root = exe_dir(&["activation", "echo", "echo_provider", "faulty"]);
    let bus = CallBus::new(BusOptions::new(
        root.path().to_path_buf(),
        PathBuf::from("/tmp"),
        env_with_marker(),
    ))
    .await
    .expect("bus");
    (root, bus)
}

#[tokio::test]
async fn lists_enabled_plugins_and_echoes() {
    let (_root, bus) = bus().await;
    let plugins = bus.plugins().await;
    for name in ["activation", "echo", "echo_provider", "faulty"] {
        let plugin = plugins.iter().find(|plugin| plugin.name == name).expect(name);
        assert!(matches!(plugin.status, PluginStatus::Enabled));
    }
    let echoed = bus
        .call("echo.ping", serde_json::json!({"value": "ping"}))
        .await
        .expect("ping");
    assert_eq!(echoed["value"], "ping");
    let asked = bus
        .call("echo.ask", serde_json::json!({"value": "ping"}))
        .await
        .expect("ask");
    assert_eq!(asked["value"], "pong:ping");
    let marker = bus
        .call("echo.marker", serde_json::json!({}))
        .await
        .expect("marker");
    assert_eq!(marker["value"], "marker");
    bus.close().await;
}

#[tokio::test]
async fn disable_hides_plugin_and_undeclared_code_passes() {
    let (_root, bus) = bus().await;
    bus.call(
        "activation.set_disabled",
        serde_json::json!({"disabled": ["echo"]}),
    )
    .await
    .expect("disable");
    let error = bus
        .call("echo.ping", serde_json::json!({"value": "ping"}))
        .await
        .expect_err("disabled");
    assert_eq!(error.code, -32001);
    let undeclared = bus
        .call("faulty.undeclared", Value::Object(serde_json::Map::new()))
        .await
        .expect_err("undeclared");
    assert_eq!(undeclared.code, 99);
    let trap = bus
        .call("faulty.trap", Value::Object(serde_json::Map::new()))
        .await
        .expect_err("trap");
    assert_eq!(trap.code, -32603);
    let plugins = bus.plugins().await;
    let faulty = plugins.iter().find(|plugin| plugin.name == "faulty").expect("faulty");
    assert!(matches!(faulty.status, PluginStatus::Failed { .. }));
    bus.close().await;
}
