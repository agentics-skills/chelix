//! Tests for the tool schema rules shared by every OpenAI-compatible API.
//!
//! They mirror `vscode-copilot-chat`'s `toolSchemaNormalizer.spec.ts`, with the
//! difference that an unrepresentable schema is an error here instead of a
//! silently dropped tool.

use {
    super::super::{
        schema_normalization::{normalize_tool_parameters, tool_identity},
        to_openai_tools, to_responses_api_tools,
    },
    serde_json::{Value, json},
};

fn normalize(mut schema: Value) -> Value {
    normalize_tool_parameters(&mut schema)
        .unwrap_or_else(|error| panic!("schema should normalize: {error:#}"));
    schema
}

fn normalization_error(mut schema: Value) -> String {
    normalize_tool_parameters(&mut schema)
        .err()
        .unwrap_or_else(|| panic!("schema should be refused"))
        .to_string()
}

#[test]
fn accepts_a_plain_object_schema_unchanged() {
    let schema = json!({
        "type": "object",
        "properties": {
            "path": { "type": "string", "description": "Absolute file path." },
            "lines": { "type": "array", "items": { "type": "integer" } }
        },
        "required": ["path"]
    });

    assert_eq!(normalize(schema.clone()), schema);
}

#[test]
fn rejects_a_type_the_meta_schema_does_not_define() {
    let error = normalization_error(json!({
        "type": "object",
        "properties": { "note": { "type": "text" } }
    }));

    assert!(
        error.contains("JSON Schema"),
        "the meta-schema failure should be reported: {error}"
    );
}

#[test]
fn rejects_a_required_list_that_is_not_an_array() {
    let error = normalization_error(json!({
        "type": "object",
        "properties": { "path": { "type": "string" } },
        "required": "path"
    }));

    assert!(
        error.contains("JSON Schema"),
        "the meta-schema failure should be reported: {error}"
    );
}

#[test]
fn rejects_parameters_that_are_not_an_object_schema() {
    assert!(normalization_error(json!({ "type": "string" })).contains("\"type\": \"object\""));
    assert!(normalization_error(json!({ "properties": {} })).contains("must declare"));
    // `true` is the JSON Schema that accepts anything, so it passes the
    // meta-schema and is only caught by the object-root rule.
    assert!(normalization_error(json!(true)).contains("must be a JSON object schema"));
    // An array is not a schema document at all.
    assert!(normalization_error(json!(["object"])).contains("JSON Schema"));
}

/// A tool that takes no arguments is written as `{"type": "object"}`. The key
/// has to be present on the wire, and an empty map says the same thing.
#[test]
fn adds_an_empty_property_map_when_the_root_omits_one() {
    assert_eq!(
        normalize(json!({ "type": "object", "additionalProperties": false })),
        json!({ "type": "object", "properties": {}, "additionalProperties": false })
    );
}

#[test]
fn rejects_composite_keywords_at_the_root() {
    for keyword in ["oneOf", "anyOf", "allOf", "not", "if"] {
        let error = normalization_error(json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            keyword: if keyword == "not" || keyword == "if" {
                json!({ "type": "object", "properties": {} })
            } else {
                json!([{ "type": "object", "properties": {} }])
            }
        }));
        assert!(
            error.contains(&format!("`{keyword}` at the root")),
            "`{keyword}` should be refused at the root: {error}"
        );
    }
}

#[test]
fn rejects_an_array_without_items_at_any_depth() {
    let root = normalization_error(json!({
        "type": "object",
        "properties": { "tags": { "type": "array" } }
    }));
    assert!(root.contains("array type must have items"), "{root}");

    let nested = normalization_error(json!({
        "type": "object",
        "properties": {
            "rows": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": { "cells": { "type": "array" } }
                }
            }
        }
    }));
    assert!(nested.contains("array type must have items"), "{nested}");

    let in_union = normalization_error(json!({
        "type": "object",
        "properties": {
            "value": {
                "anyOf": [{ "type": "string" }, { "type": "array" }]
            }
        }
    }));
    assert!(
        in_union.contains("array type must have items"),
        "{in_union}"
    );
}

#[test]
fn rewrites_tuple_items_as_a_union() {
    let normalized = normalize(json!({
        "type": "object",
        "properties": {
            "pair": {
                "type": "array",
                "items": [{ "type": "string" }, { "type": "integer" }]
            }
        }
    }));

    assert_eq!(
        normalized["properties"]["pair"]["items"],
        json!({ "anyOf": [{ "type": "string" }, { "type": "integer" }] })
    );
}

#[test]
fn keeps_a_single_item_schema_as_is() {
    let normalized = normalize(json!({
        "type": "object",
        "properties": {
            "names": { "type": "array", "items": { "type": "string" } }
        }
    }));

    assert_eq!(
        normalized["properties"]["names"]["items"],
        json!({ "type": "string" })
    );
}

#[test]
fn drops_required_entries_with_no_matching_property() {
    let normalized = normalize(json!({
        "type": "object",
        "properties": { "path": { "type": "string" } },
        "required": ["path", "ghost"]
    }));

    assert_eq!(normalized["required"], json!(["path"]));
}

#[test]
fn drops_orphaned_required_entries_inside_array_items() {
    let normalized = normalize(json!({
        "type": "object",
        "properties": {
            "edits": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "old_string": { "type": "string" },
                        "new_string": { "type": "string" }
                    },
                    "required": ["old_string", "new_string", "ghost"]
                }
            }
        },
        "required": ["edits"]
    }));

    assert_eq!(
        normalized["properties"]["edits"]["items"]["required"],
        json!(["old_string", "new_string"])
    );
    assert_eq!(normalized["required"], json!(["edits"]));
}

/// Unions reach the model intact. Collapsing them to one branch silently
/// changed the contract the tool advertises.
#[test]
fn preserves_nested_unions() {
    let schema = json!({
        "type": "object",
        "properties": {
            "schedule": {
                "oneOf": [
                    {
                        "type": "object",
                        "properties": { "cron": { "type": "string" } },
                        "required": ["cron"]
                    },
                    {
                        "type": "object",
                        "properties": { "every_ms": { "type": "integer" } },
                        "required": ["every_ms"]
                    }
                ]
            }
        },
        "required": ["schedule"]
    });

    assert_eq!(normalize(schema.clone()), schema);
}

#[test]
fn preserves_optional_properties() {
    let schema = json!({
        "type": "object",
        "properties": {
            "path": { "type": "string" },
            "limit": { "type": "integer", "description": "Optional row cap." }
        },
        "required": ["path"]
    });

    assert_eq!(normalize(schema.clone()), schema);
}

#[test]
fn tool_identity_requires_a_name_and_a_description() {
    let (name, description) = tool_identity(&json!({ "name": "read", "description": "Read it." }))
        .unwrap_or_else(|error| panic!("valid tool: {error:#}"));
    assert_eq!(name, "read");
    assert_eq!(description, "Read it.");

    assert!(tool_identity(&json!({ "description": "No name." })).is_err());
    assert!(tool_identity(&json!({ "name": "read" })).is_err());
    assert!(tool_identity(&json!({ "name": "read", "description": "  " })).is_err());
}

fn sample_tool() -> Value {
    json!({
        "name": "multi_edit",
        "description": "Apply sequential edits to one file.",
        "parameters": {
            "type": "object",
            "properties": {
                "file_path": { "type": "string" },
                "edits": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_string": { "type": "string" },
                            "new_string": { "type": "string" }
                        },
                        "required": ["old_string", "new_string"]
                    }
                }
            },
            "required": ["file_path", "edits", "ghost"]
        }
    })
}

#[test]
fn chat_completions_conversion_wraps_the_normalized_schema() {
    let converted = to_openai_tools(&[sample_tool()])
        .unwrap_or_else(|error| panic!("conversion should succeed: {error:#}"));

    assert_eq!(converted.len(), 1);
    let function = &converted[0]["function"];
    assert_eq!(converted[0]["type"], "function");
    assert_eq!(function["name"], "multi_edit");
    assert_eq!(function["strict"], false);
    assert_eq!(
        function["parameters"]["required"],
        json!(["file_path", "edits"])
    );
}

#[test]
fn responses_conversion_flattens_the_tool_entry() {
    let converted = to_responses_api_tools(&[sample_tool()])
        .unwrap_or_else(|error| panic!("conversion should succeed: {error:#}"));

    assert_eq!(converted.len(), 1);
    let tool = &converted[0];
    assert_eq!(tool["type"], "function");
    assert_eq!(tool["name"], "multi_edit");
    assert_eq!(
        tool["parameters"]["required"],
        json!(["file_path", "edits"])
    );
}

/// A refused schema must name the tool, otherwise a failing request gives no
/// clue which of the enabled tools is at fault.
#[test]
fn conversion_errors_name_the_offending_tool() {
    let broken = json!({
        "name": "broken",
        "description": "Has an array without items.",
        "parameters": {
            "type": "object",
            "properties": { "tags": { "type": "array" } }
        }
    });

    for error in [
        to_openai_tools(std::slice::from_ref(&broken)).err(),
        to_responses_api_tools(&[broken]).err(),
    ] {
        let error = error.unwrap_or_else(|| panic!("the broken tool must be refused"));
        assert!(
            format!("{error:#}").contains("tool `broken`"),
            "error should name the tool: {error:#}"
        );
    }
}
