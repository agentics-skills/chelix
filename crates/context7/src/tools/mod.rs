//! `context7_*` agent tools.

mod get_library_docs;
mod request;
mod resolve_library_id;

pub use {
    get_library_docs::Context7GetLibraryDocsTool, resolve_library_id::Context7ResolveLibraryIdTool,
};

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
