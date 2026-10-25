//! Guest world bindings and the `plugin!` macro.
#![allow(unsafe_code)]

wit_bindgen::generate!({
    world: "chelix:plugin/plugin",
    path: "wit",
    pub_export_macro: true,
    export_macro_name: "export_plugin",
});

pub use wit_bindgen::spawn_local;
pub use crate::plugin;

use std::rc::Rc;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::jsonrpc::{self, Request, Response, RpcError};

pub struct Host;

pub struct PluginContext<S> {
    state: Rc<S>,
}

impl Host {
    pub async fn call(&self, method: &str, params: &impl Serialize) -> Result<Value, RpcError> {
        let params = serde_json::to_value(params)
            .map_err(|error| RpcError::new(jsonrpc::INTERNAL_ERROR, error.to_string()))?;
        let request = Request::new(1, method, params);
        let text = jsonrpc::encode(&request)?;
        let response = chelix::plugin::host::call(&text).await;
        decode_result(&response)
    }

    pub async fn call_typed<R: DeserializeOwned>(
        &self,
        method: &str,
        params: &impl Serialize,
    ) -> Result<R, RpcError> {
        let value = self.call(method, params).await?;
        serde_json::from_value(value)
            .map_err(|error| RpcError::new(jsonrpc::INTERNAL_ERROR, error.to_string()))
    }

    pub async fn publish_queue(&self, method: &str, payload: &impl Serialize) -> Result<u32, String> {
        self.publish(method, payload, chelix::plugin::host::Delivery::Queue)
            .await
    }

    pub async fn publish(
        &self,
        method: &str,
        payload: &impl Serialize,
        delivery: chelix::plugin::host::Delivery,
    ) -> Result<u32, String> {
        let payload = serde_json::to_value(payload).map_err(|error| error.to_string())?;
        let notification = jsonrpc::Notification {
            jsonrpc: "2.0".to_owned(),
            method: method.to_owned(),
            params: payload,
        };
        let text = jsonrpc::encode(&notification).map_err(|error| error.to_string())?;
        chelix::plugin::host::publish(&text, delivery)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn sleep(&self, duration_ms: u64) {
        chelix::plugin::host::sleep(duration_ms).await;
    }

    pub fn log_error(&self, message: &str) {
        chelix::plugin::host::log(chelix::plugin::host::Level::Error, message);
    }

    pub fn log_info(&self, message: &str) {
        chelix::plugin::host::log(chelix::plugin::host::Level::Info, message);
    }

    pub fn providers(&self, point: &str) -> Result<Vec<crate::info::ProviderInfo>, RpcError> {
        let text = chelix::plugin::host::providers(point);
        serde_json::from_str(&text)
            .map_err(|error| RpcError::new(jsonrpc::INTERNAL_ERROR, error.to_string()))
    }

    pub async fn invoke(
        &self,
        point: &str,
        plugin: &str,
        params: &impl Serialize,
    ) -> Result<Value, RpcError> {
        let params = serde_json::to_value(params)
            .map_err(|error| RpcError::new(jsonrpc::INTERNAL_ERROR, error.to_string()))?;
        let request = Request::new(1, point, params);
        let text = jsonrpc::encode(&request)?;
        let response = chelix::plugin::host::invoke(point, plugin, &text).await;
        decode_result(&response)
    }

    pub async fn bind(&self, resource: &str, path: &str) -> Result<(), String> {
        chelix::plugin::host::bind(resource, path)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn read(&self, resource: &str, path: &str) -> Result<Vec<u8>, String> {
        chelix::plugin::host::read(resource, path)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn atomic_write(&self, resource: &str, path: &str, contents: &[u8]) -> Result<(), String> {
        chelix::plugin::host::atomic_write(resource, path, contents)
            .await
            .map_err(|error| error.to_string())
    }
}

impl<S> PluginContext<S> {
    #[must_use]
    pub fn from_state(state: Rc<S>) -> Self {
        Self { state }
    }

    #[must_use]
    pub fn state(&self) -> Rc<S> {
        Rc::clone(&self.state)
    }

    pub async fn call(&self, method: &str, params: &impl Serialize) -> Result<Value, RpcError> {
        (&Host).call(method, params).await
    }

    pub async fn call_typed<R: DeserializeOwned>(
        &self,
        method: &str,
        params: &impl Serialize,
    ) -> Result<R, RpcError> {
        (&Host).call_typed(method, params).await
    }

    pub async fn publish_queue(&self, method: &str, payload: &impl Serialize) -> Result<u32, String> {
        (&Host).publish_queue(method, payload).await
    }

    pub async fn publish(
        &self,
        method: &str,
        payload: &impl Serialize,
        delivery: chelix::plugin::host::Delivery,
    ) -> Result<u32, String> {
        (&Host).publish(method, payload, delivery).await
    }

    pub async fn sleep(&self, duration_ms: u64) {
        (&Host).sleep(duration_ms).await;
    }

    pub fn log_error(&self, message: &str) {
        (&Host).log_error(message);
    }

    pub fn log_info(&self, message: &str) {
        (&Host).log_info(message);
    }

    pub fn providers(&self, point: &str) -> Result<Vec<crate::info::ProviderInfo>, RpcError> {
        (&Host).providers(point)
    }

    pub async fn invoke(
        &self,
        point: &str,
        plugin: &str,
        params: &impl Serialize,
    ) -> Result<Value, RpcError> {
        (&Host).invoke(point, plugin, params).await
    }

    pub async fn bind(&self, resource: &str, path: &str) -> Result<(), String> {
        (&Host).bind(resource, path).await
    }

    pub async fn read(&self, resource: &str, path: &str) -> Result<Vec<u8>, String> {
        (&Host).read(resource, path).await
    }

    pub async fn atomic_write(&self, resource: &str, path: &str, contents: &[u8]) -> Result<(), String> {
        (&Host).atomic_write(resource, path, contents).await
    }
}

fn decode_result(response: &str) -> Result<Value, RpcError> {
    let value: Value = serde_json::from_str(response)
        .map_err(|error| RpcError::new(jsonrpc::INTERNAL_ERROR, error.to_string()))?;
    if let Some(error) = value.get("error") {
        return match serde_json::from_value(error.clone()) {
            Ok(error) => Err(error),
            Err(error) => Err(RpcError::new(
                jsonrpc::INTERNAL_ERROR,
                format!("response error is invalid: {error}"),
            )),
        };
    }
    value
        .get("result")
        .cloned()
        .ok_or_else(|| RpcError::new(jsonrpc::INTERNAL_ERROR, "response has no result"))
}

pub fn response_json(id: Option<Value>, body: Result<Value, RpcError>) -> String {
    let response = match body {
        Ok(result) => Response::ok(id, result),
        Err(error) => Response::err(id, error),
    };
    jsonrpc::encode(&response).unwrap_or_else(|error| {
        format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{{\"code\":-32603,\"message\":\"{}\"}}}}",
            error.message.replace('"', "'")
        )
    })
}

pub fn schema_of<T: schemars::JsonSchema>() -> Value {
    let generator = schemars::generate::SchemaSettings::draft07().into_generator();
    match serde_json::to_value(generator.into_root_schema_for::<T>()) {
        Ok(value) => value,
        Err(error) => Value::String(error.to_string()),
    }
}

pub fn method_object(
    name: &str,
    params_schema: Value,
    result_schema: Value,
    errors: &[(i64, &str)],
    example_params: Value,
    example_result: Value,
) -> Value {
    let properties = params_schema
        .get("properties")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let required = params_schema
        .get("required")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut descriptors = Vec::new();
    if let Some(properties) = properties.as_object() {
        for (field, schema) in properties {
            let is_required = required.iter().any(|item| item.as_str() == Some(field));
            descriptors.push(serde_json::json!({
                "name": field,
                "schema": schema,
                "required": is_required,
            }));
        }
    }
    let errors = errors
        .iter()
        .map(|(code, message)| serde_json::json!({ "code": code, "message": message }))
        .collect::<Vec<_>>();
    serde_json::json!({
        "name": name,
        "paramStructure": "by-name",
        "params": descriptors,
        "result": { "name": "result", "schema": result_schema },
        "errors": errors,
        "examples": [{
            "name": "example",
            "params": [{ "name": "params", "value": example_params }],
            "result": { "name": "result", "value": example_result }
        }],
    })
}

/// Builds the OpenRPC document stored by a plugin.
#[must_use]
pub fn document_value(
    name: &str,
    api: &str,
    version: &str,
    description: &str,
    protocol: u64,
    extension: Value,
    methods: Vec<Value>,
) -> Value {
    let mut document = serde_json::json!({
        "openrpc": "1.4.0",
        "info": { "title": api, "version": version, "description": description },
        "methods": methods,
    });
    if let (Some(object), Some(extension)) = (document.as_object_mut(), extension.as_object()) {
        object.insert("x-chelix-plugin".into(), serde_json::json!({
            "name": name,
            "protocol": protocol,
        }));
        for (key, value) in extension {
            if key == "x-chelix-plugin" {
                if let (Some(plugin), Some(extra)) = (
                    object.get_mut("x-chelix-plugin").and_then(Value::as_object_mut),
                    value.as_object(),
                ) {
                    for (extra_key, extra_value) in extra {
                        plugin.insert(extra_key.clone(), extra_value.clone());
                    }
                }
            } else {
                object.insert(key.clone(), value.clone());
            }
        }
    }
    hoist_definitions(&mut document, &format!("urn:chelix:plugin:{name}"));
    document
}

fn hoist_definitions(value: &mut Value, urn: &str) {
    let mut schemas = Map::new();
    collect_definitions(value, &mut schemas);
    rewrite_refs(value, urn);
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "components".into(),
            serde_json::json!({ "schemas": schemas }),
        );
    }
}

fn collect_definitions(value: &mut Value, schemas: &mut Map<String, Value>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_definitions(item, schemas);
            }
        },
        Value::Object(object) => {
            if let Some(Value::Object(definitions)) = object.remove("definitions") {
                for (name, mut schema) in definitions {
                    collect_definitions(&mut schema, schemas);
                    schemas.insert(name, schema);
                }
            }
            for child in object.values_mut() {
                collect_definitions(child, schemas);
            }
        },
        _ => {},
    }
}

fn rewrite_refs(value: &mut Value, urn: &str) {
    match value {
        Value::Array(items) => {
            for item in items {
                rewrite_refs(item, urn);
            }
        },
        Value::Object(object) => {
            for child in object.values_mut() {
                rewrite_refs(child, urn);
            }
        },
        Value::String(text) if text.starts_with("#/definitions/") => {
            let name = &text["#/definitions/".len()..];
            *text = format!("{urn}#/components/schemas/{name}");
        },
        _ => {},
    }
}
