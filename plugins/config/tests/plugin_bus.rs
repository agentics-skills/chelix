#![cfg(not(target_family = "wasm"))]

use std::collections::HashMap;
use std::path::PathBuf;

use chelix_call_bus::testing::exe_dir;
use chelix_call_bus::{BusOptions, CallBus};

#[tokio::test]
async fn get_missing_path_is_null_and_patch_writes() {
    let root = exe_dir(&["config"]);
    let dir = root.path().join("cfg");
    std::fs::create_dir_all(&dir).expect("cfg");
    let file = dir.join("chelix.toml");
    std::fs::write(&file, "key = 1\n").expect("write");
    let mut env = HashMap::new();
    env.insert("CHELIX_CONFIG_DIR".to_owned(), dir.to_string_lossy().to_string());
    env.insert("HOME".to_owned(), "/tmp".to_owned());
    let bus = CallBus::new(BusOptions::new(root.path().to_path_buf(), PathBuf::from("/tmp"), env))
        .await
        .expect("bus");
    let missing = bus
        .call("config.get", serde_json::json!({"path": "missing"}))
        .await
        .expect("get");
    assert!(missing["value"].is_null());
    let patched = bus
        .call(
            "config.patch",
            serde_json::json!({"ops": [{"op": "set", "path": "section.key", "value": 7}]}),
        )
        .await
        .expect("patch");
    assert_eq!(patched["changed"], true);
    let text = std::fs::read_to_string(&file).expect("read");
    assert!(text.contains("key = 7"));
    bus.close().await;
}
