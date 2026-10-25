//! Addresses and serde types for the config API.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const CONFIG_GET: &str = "config.get";
pub const CONFIG_PATCH: &str = "config.patch";
pub const CHANGED: &str = "config.changed";
pub const RELOAD: &str = "bus.plugins.reload";

pub const INVALID_PATH: i64 = 1;
pub const INVALID_VALUE: i64 = 2;
pub const PATH_CONFLICT: i64 = 3;
pub const INVALID_CANDIDATE: i64 = 4;
pub const IO: i64 = 5;

fn example_get_params() -> ConfigGetParams {
    ConfigGetParams {
        path: "section.key".into(),
    }
}

fn example_get_result() -> ConfigGetResult {
    ConfigGetResult {
        revision: 3,
        value: Some(Value::from(42)),
    }
}

fn example_patch_params() -> ConfigPatchParams {
    ConfigPatchParams {
        ops: vec![PatchOp::Set {
            path: "section.key".into(),
            value: Value::from(42),
        }],
    }
}

fn example_patch_result() -> ConfigPatchResult {
    ConfigPatchResult {
        revision: 4,
        changed: true,
    }
}

fn example_changed() -> ConfigChanged {
    ConfigChanged {
        revision: 4,
        sections: vec!["section".into()],
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(example = example_get_params())]
pub struct ConfigGetParams {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(example = example_get_result())]
pub struct ConfigGetResult {
    pub revision: u64,
    pub value: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PatchOp {
    Set { path: String, value: Value },
    Remove { path: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(example = example_patch_params())]
pub struct ConfigPatchParams {
    pub ops: Vec<PatchOp>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(example = example_patch_result())]
pub struct ConfigPatchResult {
    pub revision: u64,
    pub changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(example = example_changed())]
pub struct ConfigChanged {
    pub revision: u64,
    pub sections: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ActivationParams {}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ActivationResult {
    pub disabled: Vec<String>,
}

pub fn get_example() -> (Value, Value) {
    (
        serde_json::json!({"path": "section.key"}),
        serde_json::json!({"revision": 3, "value": 42}),
    )
}

pub fn patch_example() -> (Value, Value) {
    (
        serde_json::json!({"ops": [{"op": "set", "path": "section.key", "value": 42}]}),
        serde_json::json!({"revision": 4, "changed": true}),
    )
}
