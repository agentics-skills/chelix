use std::collections::HashMap;
use std::path::PathBuf;

use chelix_call_bus::info::PluginStatus;
use chelix_call_bus::testing::exe_dir;
use chelix_call_bus::{BusOptions, CallBus};

#[tokio::test]
async fn product_plugins_start_enabled() {
    let product = concat!(env!("CARGO_MANIFEST_DIR"), "/../../plugins");
    let mut names = Vec::new();
    let entries = std::fs::read_dir(product).expect("product plugins");
    for entry in entries {
        let entry = entry.expect("entry");
        if entry.path().is_dir() {
            names.push(entry.file_name().to_string_lossy().to_string());
        }
    }
    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let root = exe_dir(&name_refs);
    let config_dir = root.path().join("cfg");
    std::fs::create_dir_all(&config_dir).expect("cfg");
    std::fs::write(
        config_dir.join("chelix.toml"),
        "[tools]\nmax_tool_result_bytes = 50000\n[tools.execute_command]\nterminal_size = \"115x58\"\n[sandbox]\narchived_session_retention_days = 7\n",
    )
    .expect("config");
    let mut env = HashMap::new();
    env.insert(
        "CHELIX_CONFIG_DIR".to_owned(),
        config_dir.to_string_lossy().to_string(),
    );
    env.insert("HOME".to_owned(), "/tmp".to_owned());
    let bus = CallBus::new(BusOptions::new(
        root.path().to_path_buf(),
        PathBuf::from("/tmp"),
        env,
    ))
    .await
    .expect("bus");
    let plugins = bus.plugins().await;
    assert!(!plugins.is_empty());
    for plugin in &plugins {
        assert!(
            matches!(plugin.status, PluginStatus::Enabled),
            "{} is {:?}",
            plugin.name,
            plugin.status
        );
    }
    bus.close().await;
}
