//! `github_*` agent tools.

mod get_file_contents;
mod search_code;

pub use {get_file_contents::GithubGetFileContentsTool, search_code::GithubSearchCodeTool};

use serde_json::Value;

use crate::error::{Error, Result};

/// Deserialize enriched tool parameters after dropping the runner's internal
/// underscore-prefixed context fields.
fn parse_params<T: serde::de::DeserializeOwned>(tool: &str, mut params: Value) -> Result<T> {
    let map = params
        .as_object_mut()
        .ok_or_else(|| Error::message(format!("{tool} parameters must be an object")))?;
    map.retain(|key, _| !key.starts_with('_'));
    serde_json::from_value(params)
        .map_err(|error| Error::message(format!("invalid {tool} parameters: {error}")))
}
