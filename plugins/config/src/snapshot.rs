//! In-memory snapshot and `config.get`.

use std::cell::RefCell;
use std::rc::Rc;

use chelix_call_bus::guest::PluginContext;
use chelix_call_bus::jsonrpc::RpcError;
use serde_json::{Map, Value};
use toml::value::Datetime;
use toml::Value as Toml;

use crate::contract::{ConfigGetParams, ConfigGetResult, INVALID_PATH};

#[derive(Clone)]
pub struct Snapshot {
    pub revision: u64,
    pub root: toml::Table,
}

#[derive(Clone)]
pub struct ConfigState {
    pub file: String,
    pub current: Rc<RefCell<Rc<Snapshot>>>,
    pub queue: crate::queue::QueueHandle,
    pub shutdown: crate::queue::Shutdown,
}

pub fn publish(state: &ConfigState, root: toml::Table) -> u64 {
    let revision = state.current.borrow().revision.saturating_add(1);
    *state.current.borrow_mut() = Rc::new(Snapshot { revision, root });
    revision
}

pub fn current(state: &ConfigState) -> Rc<Snapshot> {
    Rc::clone(&state.current.borrow())
}

pub async fn get(
    ctx: &PluginContext<ConfigState>,
    params: ConfigGetParams,
) -> Result<ConfigGetResult, RpcError> {
    let state = ctx.state();
    let snapshot = current(&state);
    let value = lookup(&snapshot.root, &params.path).map_err(|error| {
        RpcError::new(INVALID_PATH, format!("{}: {error}", state.file)).with_data(serde_json::json!({
            "path": params.path,
        }))
    })?;
    Ok(ConfigGetResult {
        revision: snapshot.revision,
        value,
    })
}

pub fn lookup(root: &toml::Table, path: &str) -> Result<Option<Value>, String> {
    let keys = toml_edit::Key::parse(path).map_err(|error| error.to_string())?;
    let parts: Vec<&str> = keys.iter().map(toml_edit::Key::get).collect();
    if parts.is_empty() {
        return Err("empty path".into());
    }
    let (last, parents) = parts.split_last().ok_or_else(|| "empty path".to_owned())?;
    let mut table = root;
    for part in parents {
        let Some(value) = table.get(part) else {
            return Ok(None);
        };
        let Some(next) = value.as_table() else {
            return Err(format!("{part} is not a table"));
        };
        table = next;
    }
    match table.get(*last) {
        None => Ok(None),
        Some(value) => toml_to_json(value).map(Some),
    }
}

fn toml_to_json(value: &Toml) -> Result<Value, String> {
    match value {
        Toml::String(text) => Ok(Value::String(text.clone())),
        Toml::Integer(number) => Ok(Value::from(*number)),
        Toml::Float(number) => serde_json::Number::from_f64(*number)
            .map(Value::Number)
            .ok_or_else(|| "not a json number".to_owned()),
        Toml::Boolean(flag) => Ok(Value::Bool(*flag)),
        Toml::Datetime(datetime) => Ok(Value::String(datetime_to_rfc3339(datetime))),
        Toml::Array(items) => {
            let mut values = Vec::with_capacity(items.len());
            for item in items {
                values.push(toml_to_json(item)?);
            }
            Ok(Value::Array(values))
        },
        Toml::Table(table) => {
            let mut object = Map::new();
            for (key, item) in table {
                object.insert(key.clone(), toml_to_json(item)?);
            }
            Ok(Value::Object(object))
        },
    }
}

fn datetime_to_rfc3339(datetime: &Datetime) -> String {
    datetime.to_string()
}

pub async fn activation(
    ctx: &PluginContext<ConfigState>,
    _params: crate::contract::ActivationParams,
) -> Result<crate::contract::ActivationResult, RpcError> {
    let state = ctx.state();
    let snapshot = current(&state);
    let disabled = match snapshot.root.get("plugins").and_then(|value| value.as_table()) {
        None => Vec::new(),
        Some(plugins) => match plugins.get("disabled") {
            None => Vec::new(),
            Some(Toml::Array(items)) => items
                .iter()
                .map(|item| {
                    item.as_str().map(str::to_owned).ok_or_else(|| {
                        RpcError::new(INVALID_PATH, format!("{}: plugins.disabled", state.file))
                            .with_data(serde_json::json!({"path": "plugins.disabled"}))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
            Some(_) => {
                return Err(RpcError::new(
                    INVALID_PATH,
                    format!("{}: plugins.disabled", state.file),
                )
                .with_data(serde_json::json!({"path": "plugins.disabled"})))
            },
        },
    };
    Ok(crate::contract::ActivationResult { disabled })
}
