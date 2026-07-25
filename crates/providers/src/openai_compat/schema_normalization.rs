//! Normalization of tool parameter schemas for OpenAI-compatible APIs.
//!
//! The rules mirror `vscode-copilot-chat`'s `toolSchemaNormalizer`, which has
//! been validated against production traffic for years. Two deliberate
//! differences:
//!
//! * No rule is gated on a model name or model family. Chelix targets the
//!   OpenAI function-calling standard, so every schema is treated identically.
//! * A schema that cannot be represented is an error, not a silently dropped
//!   tool. Callers propagate it and refuse the request.

use {
    anyhow::{Result, bail},
    serde_json::{Map, Value},
};

/// Keywords that carry a nested schema as a single value.
const NESTED_SCHEMA_KEYWORDS: &[&str] = &["not", "if", "then", "else", "contains", "propertyNames"];

/// Keywords whose value is an array of schemas.
const SCHEMA_ARRAY_KEYWORDS: &[&str] = &["anyOf", "oneOf", "allOf"];

/// Keywords whose value maps names to schemas.
const SCHEMA_MAP_KEYWORDS: &[&str] = &[
    "properties",
    "patternProperties",
    "dependencies",
    "definitions",
    "$defs",
];

/// Validate and normalize one tool's parameter schema in place.
///
/// Returns an error when the schema cannot be expressed in the OpenAI
/// function-calling dialect. The caller names the offending tool.
pub(crate) fn normalize_tool_parameters(schema: &mut Value) -> Result<()> {
    validate_against_meta_schema(schema)?;
    ensure_object_root(schema)?;
    ensure_property_map(schema);
    reject_top_level_composites(schema)?;
    reject_arrays_without_items(schema)?;
    replace_tuple_items(schema);
    prune_orphaned_required(schema);
    Ok(())
}

/// Validate a schema document against the JSON Schema meta-schema.
///
/// Catches malformed schemas — a misspelled `"type": "text"`, a `required`
/// that is not an array — before the request reaches the provider, which
/// otherwise answers with an opaque 400.
///
/// Draft 7 is the dialect the reference validates against, and the one tool
/// authors write: it still allows the tuple form of `items`, which
/// [`replace_tuple_items`] rewrites afterwards.
fn validate_against_meta_schema(schema: &Value) -> Result<()> {
    let Err(error) = jsonschema::draft7::meta::validate(schema) else {
        return Ok(());
    };
    let path = error.instance_path().to_string();
    let location = if path.is_empty() {
        "the schema root".to_string()
    } else {
        format!("`{path}`")
    };
    bail!("tool parameters do not match JSON Schema at {location}: {error}");
}

/// Require `parameters` to describe an object.
///
/// OpenAI models call functions with a JSON object of named arguments, so a
/// schema of any other shape has no valid instance.
fn ensure_object_root(schema: &Value) -> Result<()> {
    let Some(object) = schema.as_object() else {
        bail!("tool parameters must be a JSON object schema");
    };
    match object.get("type").and_then(Value::as_str) {
        Some("object") => {},
        Some(other) => bail!("tool parameters must have `\"type\": \"object\"`, found `{other}`"),
        None => bail!("tool parameters must declare `\"type\": \"object\"`"),
    }
    Ok(())
}

/// Add an empty `properties` map when the root omits it.
///
/// `{"type": "object"}` is a valid schema meaning "any object", which is how a
/// tool that takes no arguments is written, but the API expects the key to be
/// present. An empty map constrains nothing, so this states the same contract
/// rather than changing it.
fn ensure_property_map(schema: &mut Value) {
    let Some(object) = schema.as_object_mut() else {
        return;
    };
    if !object.get("properties").is_some_and(Value::is_object) {
        object.insert("properties".to_string(), Value::Object(Map::new()));
    }
}

/// Refuse composite keywords beside the root `properties`.
///
/// Providers reject a top-level `oneOf`/`if`/… on tool parameters. The
/// restriction is on the root only: the same keywords nested under a property
/// are part of the contract and are passed through untouched.
fn reject_top_level_composites(schema: &Value) -> Result<()> {
    let Some(object) = schema.as_object() else {
        return Ok(());
    };
    for keyword in SCHEMA_ARRAY_KEYWORDS.iter().chain(NESTED_SCHEMA_KEYWORDS) {
        if object.contains_key(*keyword) {
            bail!("tool parameters must not use `{keyword}` at the root; move it under a property");
        }
    }
    Ok(())
}

/// Refuse `"type": "array"` without `items`.
///
/// The array's element type is then undefined, and models fill it with
/// arbitrarily shaped values.
fn reject_arrays_without_items(schema: &Value) -> Result<()> {
    visit_schema_nodes(schema, &mut |node| {
        if node.get("type").and_then(Value::as_str) == Some("array") && !node.contains_key("items")
        {
            bail!("tool parameters array type must have items");
        }
        Ok(())
    })
}

/// Rewrite draft-07 tuple `items` as `{"anyOf": [...]}`.
///
/// Draft 2020-12 — the dialect OpenAI documents — moved positional item
/// schemas to `prefixItems` and only accepts a single schema in `items`.
fn replace_tuple_items(schema: &mut Value) {
    visit_schema_nodes_mut(schema, &mut |node| {
        let Some(items) = node.get_mut("items") else {
            return;
        };
        let Some(entries) = items.as_array_mut() else {
            return;
        };
        let variants = std::mem::take(entries);
        *items = serde_json::json!({ "anyOf": variants });
    });
}

/// Drop `required` entries with no matching property.
///
/// The provider validates that every required name is declared and rejects the
/// whole request when one is missing.
fn prune_orphaned_required(schema: &mut Value) {
    visit_schema_nodes_mut(schema, &mut |node| {
        let declared: Vec<String> = node
            .get("properties")
            .and_then(Value::as_object)
            .map(|properties| properties.keys().cloned().collect())
            .unwrap_or_default();
        let Some(required) = node.get_mut("required").and_then(Value::as_array_mut) else {
            return;
        };
        required.retain(|entry| {
            entry
                .as_str()
                .is_some_and(|name| declared.iter().any(|candidate| candidate == name))
        });
    });
}

/// Walk every schema node, including the root, stopping at the first error.
fn visit_schema_nodes<F>(schema: &Value, visit: &mut F) -> Result<()>
where
    F: FnMut(&Map<String, Value>) -> Result<()>,
{
    let Some(object) = schema.as_object() else {
        return Ok(());
    };
    visit(object)?;

    for keyword in SCHEMA_MAP_KEYWORDS {
        if let Some(entries) = object.get(*keyword).and_then(Value::as_object) {
            for child in entries.values() {
                visit_schema_nodes(child, visit)?;
            }
        }
    }
    for keyword in SCHEMA_ARRAY_KEYWORDS {
        if let Some(entries) = object.get(*keyword).and_then(Value::as_array) {
            for child in entries {
                visit_schema_nodes(child, visit)?;
            }
        }
    }
    for keyword in NESTED_SCHEMA_KEYWORDS {
        if let Some(child) = object.get(*keyword) {
            visit_schema_nodes(child, visit)?;
        }
    }
    if let Some(items) = object.get("items") {
        match items.as_array() {
            Some(entries) => {
                for child in entries {
                    visit_schema_nodes(child, visit)?;
                }
            },
            None => visit_schema_nodes(items, visit)?,
        }
    }
    if let Some(entries) = object.get("prefixItems").and_then(Value::as_array) {
        for child in entries {
            visit_schema_nodes(child, visit)?;
        }
    }
    if let Some(additional) = object.get("additionalProperties") {
        visit_schema_nodes(additional, visit)?;
    }
    Ok(())
}

/// Mutable counterpart of [`visit_schema_nodes`].
///
/// Each node is visited before its children, so a rewritten `items` is itself
/// traversed afterwards.
fn visit_schema_nodes_mut<F>(schema: &mut Value, visit: &mut F)
where
    F: FnMut(&mut Map<String, Value>),
{
    let Some(object) = schema.as_object_mut() else {
        return;
    };
    visit(object);

    for keyword in SCHEMA_MAP_KEYWORDS {
        if let Some(entries) = object.get_mut(*keyword).and_then(Value::as_object_mut) {
            for child in entries.values_mut() {
                visit_schema_nodes_mut(child, visit);
            }
        }
    }
    for keyword in SCHEMA_ARRAY_KEYWORDS {
        if let Some(entries) = object.get_mut(*keyword).and_then(Value::as_array_mut) {
            for child in entries {
                visit_schema_nodes_mut(child, visit);
            }
        }
    }
    for keyword in NESTED_SCHEMA_KEYWORDS {
        if let Some(child) = object.get_mut(*keyword) {
            visit_schema_nodes_mut(child, visit);
        }
    }
    if let Some(items) = object.get_mut("items") {
        match items.as_array_mut() {
            Some(entries) => {
                for child in entries {
                    visit_schema_nodes_mut(child, visit);
                }
            },
            None => visit_schema_nodes_mut(items, visit),
        }
    }
    if let Some(entries) = object.get_mut("prefixItems").and_then(Value::as_array_mut) {
        for child in entries {
            visit_schema_nodes_mut(child, visit);
        }
    }
    if let Some(additional) = object.get_mut("additionalProperties") {
        visit_schema_nodes_mut(additional, visit);
    }
}

/// Read a tool definition's identity fields.
///
/// A tool without a description gives the model nothing to select on, and one
/// without a name cannot be called at all.
pub(crate) fn tool_identity(tool: &Value) -> Result<(String, String)> {
    let Some(name) = tool.get("name").and_then(Value::as_str) else {
        bail!("tool definition is missing a `name`");
    };
    let description = tool
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if description.trim().is_empty() {
        bail!("tool `{name}` has an empty description");
    }
    Ok((name.to_string(), description.to_string()))
}
