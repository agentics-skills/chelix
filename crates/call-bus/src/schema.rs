//! Draft 7 schema checks for plugin documents and call payloads.

use std::collections::HashMap;
use std::sync::Arc;

use jsonschema::{Draft, Registry, Resource, Validator};
use serde_json::Value;

const OPENRPC_META: &str = include_str!("../meta/openrpc-1.4.json");
const META_TOOLS: &str = include_str!("../meta/json-schema-tools.json");

pub struct SchemaCache {
    openrpc: Validator,
    instances: HashMap<String, Arc<Validator>>,
}

impl SchemaCache {
    pub fn new() -> Result<Self, String> {
        let meta_tools: Value = serde_json::from_str(META_TOOLS)
            .map_err(|error| format!("json-schema.tools meta schema: {error}"))?;
        let openrpc_meta: Value = serde_json::from_str(OPENRPC_META)
            .map_err(|error| format!("openrpc meta schema: {error}"))?;
        let registry = Registry::new()
            .add(
                "https://meta.json-schema.tools/",
                Resource::from_contents(meta_tools.clone()),
            )
            .map_err(|error| error.to_string())?
            .add(
                "https://meta.json-schema.tools",
                Resource::from_contents(meta_tools),
            )
            .map_err(|error| error.to_string())?
            .prepare()
            .map_err(|error| error.to_string())?;
        let openrpc_meta = Box::leak(Box::new(openrpc_meta));
        let registry = Box::leak(Box::new(registry));
        let openrpc = jsonschema::options()
            .with_draft(Draft::Draft7)
            .with_registry(registry)
            .build(openrpc_meta)
            .map_err(|error| format!("openrpc validator: {error}"))?;
        Ok(Self {
            openrpc,
            instances: HashMap::new(),
        })
    }

    pub fn validate_document(&self, document: &Value) -> Result<(), String> {
        self.openrpc
            .validate(document)
            .map_err(|error| format!("openrpc: {error}"))?;
        let Some(object) = document.as_object() else {
            return Err("document is not an object".into());
        };
        if let Some(Value::Array(methods)) = object.get("methods") {
            for method in methods {
                self.validate_method_schemas(method)?;
            }
        }
        if let Some(Value::Array(points)) = object.get("x-chelix-points") {
            for point in points {
                if let Some(schema) = point.get("params") {
                    validate_draft7(schema, "point.params")?;
                }
                if let Some(schema) = point.get("result") {
                    validate_draft7(schema, "point.result")?;
                }
                if let Some(schema) = point.get("meta") {
                    validate_draft7(schema, "point.meta")?;
                }
            }
        }
        if let Some(Value::Array(events)) = object.get("x-chelix-events") {
            for event in events {
                if let Some(schema) = event.get("payload") {
                    validate_draft7(schema, "event.payload")?;
                }
            }
        }
        Ok(())
    }

    fn validate_method_schemas(&self, method: &Value) -> Result<(), String> {
        let Some(params) = method.get("params").and_then(Value::as_array) else {
            return Ok(());
        };
        for param in params {
            if let Some(schema) = param.get("schema") {
                validate_draft7(schema, "method.params")?;
            }
        }
        if let Some(schema) = method.get("result").and_then(|result| result.get("schema")) {
            validate_draft7(schema, "method.result")?;
        }
        Ok(())
    }

    pub fn instance(&mut self, schema: &Value) -> Result<Arc<Validator>, String> {
        let key = serde_json::to_string(schema).map_err(|error| error.to_string())?;
        if let Some(validator) = self.instances.get(&key) {
            return Ok(Arc::clone(validator));
        }
        let validator = Arc::new(
            jsonschema::options()
                .with_draft(Draft::Draft7)
                .build(schema)
                .map_err(|error| format!("schema: {error}"))?,
        );
        self.instances.insert(key, Arc::clone(&validator));
        Ok(validator)
    }

    pub fn check_in_document(
        plugin: &str,
        document: &Value,
        schema: &Value,
        instance: &Value,
    ) -> Result<(), String> {
        let uri = format!("urn:chelix:plugin:{plugin}");
        let registry = Registry::new()
            .add(
                uri.as_str(),
                Draft::Draft7.create_resource(document.clone()),
            )
            .map_err(|error| error.to_string())?
            .prepare()
            .map_err(|error| error.to_string())?;
        let validator = jsonschema::options()
            .with_draft(Draft::Draft7)
            .with_registry(&registry)
            .build(schema)
            .map_err(|error| format!("schema: {error}"))?;
        validator
            .validate(instance)
            .map_err(|error| error.to_string())
    }
}

fn validate_draft7(schema: &Value, what: &str) -> Result<(), String> {
    jsonschema::draft7::meta::validate(schema).map_err(|error| format!("{what}: {error}"))
}
