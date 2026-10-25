//! Consumer contract for `config.get`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const CONFIG_GET: &str = "config.get";

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ConfigGetParams {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ConfigGetResult {
    pub revision: u64,
    pub value: Option<Value>,
}
