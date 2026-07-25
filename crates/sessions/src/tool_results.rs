//! Disk persistence for full tool outputs.
//!
//! Every tool call's complete output is written to
//! `<sessions_dir>/tool-results/<session>/<call>/content.txt` (line-oriented
//! text) or `content.json` + `schema.json` (structured JSON), so agents can
//! re-read full results with Read/Grep after the in-context copy is truncated.
//!
//! Modeled on VS Code Copilot Chat's `ChatDiskSessionResources` and its
//! large-tool-results-to-disk mechanism (`toJsonSchema.ts`,
//! `chatDiskSessionResourcesImpl.ts`).

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::Result;

/// Directory under the sessions dir where tool results are stored.
pub const TOOL_RESULTS_DIR_NAME: &str = "tool-results";

/// Replace every character outside `[a-zA-Z0-9_.-]` with `_` to prevent
/// path injection. Empty input maps to `unknown`.
#[must_use]
pub fn sanitize_path_component(component: &str) -> String {
    if component.is_empty() {
        return "unknown".to_string();
    }
    component
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Paths produced by persisting one tool result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedToolResult {
    /// Full output file (`content.txt` or `content.json`).
    pub content_path: PathBuf,
    /// Inferred JSON schema file, present for JSON payloads.
    pub schema_path: Option<PathBuf>,
    /// Size in bytes of the persisted content.
    pub content_bytes: usize,
}

/// Writes full tool outputs to per-session, per-call directories.
#[derive(Debug, Clone)]
pub struct ToolResultStore {
    base_dir: PathBuf,
}

impl ToolResultStore {
    /// `sessions_dir` is the same directory that backs
    /// [`crate::store::SessionStore`].
    #[must_use]
    pub fn new(sessions_dir: PathBuf) -> Self {
        Self {
            base_dir: sessions_dir.join(TOOL_RESULTS_DIR_NAME),
        }
    }

    /// Directory holding all persisted tool results for one session.
    #[must_use]
    pub fn session_dir(&self, session_key: &str) -> PathBuf {
        self.base_dir.join(sanitize_path_component(session_key))
    }

    fn call_dir(&self, session_key: &str, call_id: &str) -> PathBuf {
        self.session_dir(session_key)
            .join(sanitize_path_component(call_id))
    }

    /// Persist the full output of one tool call.
    ///
    /// Structured JSON payloads are pretty-printed to `content.json` with an
    /// inferred `schema.json` alongside; everything else goes to `content.txt`.
    /// Writes are idempotent: existing files are left untouched.
    #[tracing::instrument(skip(self, raw), fields(raw_len = raw.len()))]
    pub async fn persist(
        &self,
        session_key: &str,
        call_id: &str,
        raw: &str,
    ) -> Result<PersistedToolResult> {
        let dir = self.call_dir(session_key, call_id);
        let (file_name, content, schema) = match serde_json::from_str::<Value>(raw) {
            // A bare JSON string is plain text content, not structured data.
            Ok(Value::String(text)) => ("content.txt", text, None),
            Ok(parsed) => {
                let schema = serde_json::to_string(&to_json_schema(&parsed))?;
                // Pretty-print: friendlier to line-based offsets in the Read tool.
                let content = serde_json::to_string_pretty(&parsed)?;
                ("content.json", content, Some(schema))
            },
            Err(_) => ("content.txt", raw.to_string(), None),
        };

        tokio::fs::create_dir_all(&dir).await?;
        let content_path = dir.join(file_name);
        let content_bytes = content.len();
        write_if_missing(&content_path, content).await?;
        let schema_path = match schema {
            Some(schema) => {
                let path = dir.join("schema.json");
                write_if_missing(&path, schema).await?;
                Some(path)
            },
            None => None,
        };
        tracing::debug!(
            path = %content_path.display(),
            bytes = content_bytes,
            "persisted tool result"
        );
        Ok(PersistedToolResult {
            content_path,
            schema_path,
            content_bytes,
        })
    }

    /// Persist explicitly line-oriented content as `content.txt`, even when
    /// the text itself is valid JSON.
    #[tracing::instrument(skip(self, content), fields(content_len = content.len()))]
    pub async fn persist_text(
        &self,
        session_key: &str,
        call_id: &str,
        content: &str,
    ) -> Result<PersistedToolResult> {
        let dir = self.call_dir(session_key, call_id);
        tokio::fs::create_dir_all(&dir).await?;
        let content_path = dir.join("content.txt");
        write_if_missing(&content_path, content.to_string()).await?;
        tracing::debug!(
            path = %content_path.display(),
            bytes = content.len(),
            "persisted text tool result"
        );
        Ok(PersistedToolResult {
            content_path,
            schema_path: None,
            content_bytes: content.len(),
        })
    }
}

async fn write_if_missing(path: &Path, content: String) -> Result<()> {
    if tokio::fs::try_exists(path).await? {
        return Ok(());
    }
    tokio::fs::write(path, content).await?;
    Ok(())
}

// ── JSON schema inference (port of Copilot's toJsonSchema.ts) ──────────────

/// Infer a JSON schema describing `value`.
#[must_use]
pub fn to_json_schema(value: &Value) -> Value {
    match value {
        Value::Null => json!({ "type": "null" }),
        Value::Bool(_) => json!({ "type": "boolean" }),
        Value::Number(n) => {
            let ty = if n.is_f64() {
                "number"
            } else {
                "integer"
            };
            json!({ "type": ty })
        },
        Value::String(_) => json!({ "type": "string" }),
        Value::Array(items) => array_schema(items),
        Value::Object(map) => object_schema(map),
    }
}

fn array_schema(items: &[Value]) -> Value {
    if items.is_empty() {
        // Empty array, no item schema can be inferred.
        return json!({ "type": "array" });
    }

    if items.iter().all(Value::is_object) {
        // Merge object schemas, only common properties are required.
        let objects: Vec<&Map<String, Value>> = items.iter().filter_map(Value::as_object).collect();
        return json!({ "type": "array", "items": merge_object_schemas(&objects) });
    }

    let mut schemas = unique_schemas(items);
    if schemas.len() == 1 {
        return json!({ "type": "array", "items": schemas.remove(0) });
    }
    // Multiple different types, use oneOf.
    json!({ "type": "array", "items": { "oneOf": schemas } })
}

/// Key identifying a unique schema shape (deduplicates by `type`).
fn schema_key(schema: &Value) -> String {
    match schema.get("type").and_then(Value::as_str) {
        Some(ty) => ty.to_string(),
        None => schema.to_string(),
    }
}

/// Unique schemas for mixed values; all objects merge into one schema
/// appended after the scalar/array schemas (reference ordering).
fn unique_schemas<'a, I>(values: I) -> Vec<Value>
where
    I: IntoIterator<Item = &'a Value>,
{
    let mut keys: Vec<String> = Vec::new();
    let mut schemas: Vec<Value> = Vec::new();
    let mut objects: Vec<&Map<String, Value>> = Vec::new();

    for value in values {
        if let Some(map) = value.as_object() {
            objects.push(map);
            continue;
        }
        let schema = to_json_schema(value);
        let key = schema_key(&schema);
        if !keys.contains(&key) {
            keys.push(key);
            schemas.push(schema);
        }
    }

    if !objects.is_empty() {
        schemas.push(merge_object_schemas(&objects));
    }
    schemas
}

/// Merge multiple objects into one schema: properties are unioned and a
/// property is `required` only when present in every object.
fn merge_object_schemas(objects: &[&Map<String, Value>]) -> Value {
    let mut order: Vec<String> = Vec::new();
    let mut by_key: std::collections::HashMap<String, Vec<&Value>> =
        std::collections::HashMap::new();
    for obj in objects {
        for (key, value) in obj.iter() {
            by_key
                .entry(key.clone())
                .or_insert_with(|| {
                    order.push(key.clone());
                    Vec::new()
                })
                .push(value);
        }
    }

    let mut properties = Map::new();
    let mut required = Vec::new();
    for key in order {
        let Some(values) = by_key.get(key.as_str()) else {
            continue;
        };
        if values.len() == objects.len() {
            required.push(Value::String(key.clone()));
        }
        properties.insert(key, merge_values(values));
    }
    finish_object_schema(properties, required)
}

fn merge_values(values: &[&Value]) -> Value {
    if values.iter().all(|v| v.is_object()) {
        let objects: Vec<&Map<String, Value>> =
            values.iter().filter_map(|v| v.as_object()).collect();
        return merge_object_schemas(&objects);
    }
    let mut schemas = unique_schemas(values.iter().copied());
    if schemas.len() == 1 {
        return schemas.remove(0);
    }
    json!({ "oneOf": schemas })
}

fn object_schema(map: &Map<String, Value>) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for (key, value) in map {
        properties.insert(key.clone(), to_json_schema(value));
        // All keys present in the object are considered required.
        required.push(Value::String(key.clone()));
    }
    finish_object_schema(properties, required)
}

fn finish_object_schema(properties: Map<String, Value>, required: Vec<Value>) -> Value {
    let mut schema = Map::new();
    schema.insert("type".to_string(), json!("object"));
    schema.insert("properties".to_string(), Value::Object(properties));
    if !required.is_empty() {
        schema.insert("required".to_string(), Value::Array(required));
    }
    Value::Object(schema)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    // ── sanitize_path_component ─────────────────────────────────────────

    #[test]
    fn sanitize_keeps_safe_chars() {
        assert_eq!(
            sanitize_path_component("call_1.a-B9"),
            "call_1.a-B9".to_string()
        );
    }

    #[test]
    fn sanitize_replaces_unsafe_chars() {
        assert_eq!(
            sanitize_path_component("project:backend/../etc"),
            "project_backend_.._etc".to_string()
        );
    }

    #[test]
    fn sanitize_empty_maps_to_unknown() {
        assert_eq!(sanitize_path_component(""), "unknown".to_string());
    }

    // ── to_json_schema ──────────────────────────────────────────────────

    #[test]
    fn schema_scalars() {
        assert_eq!(to_json_schema(&json!(null)), json!({ "type": "null" }));
        assert_eq!(to_json_schema(&json!(true)), json!({ "type": "boolean" }));
        assert_eq!(to_json_schema(&json!(42)), json!({ "type": "integer" }));
        assert_eq!(to_json_schema(&json!(1.5)), json!({ "type": "number" }));
        assert_eq!(to_json_schema(&json!("hi")), json!({ "type": "string" }));
    }

    #[test]
    fn schema_empty_array() {
        assert_eq!(to_json_schema(&json!([])), json!({ "type": "array" }));
    }

    #[test]
    fn schema_homogeneous_array() {
        assert_eq!(
            to_json_schema(&json!([1, 2, 3])),
            json!({ "type": "array", "items": { "type": "integer" } })
        );
    }

    #[test]
    fn schema_mixed_array_uses_one_of() {
        let schema = to_json_schema(&json!([1, "a"]));
        assert_eq!(
            schema,
            json!({
                "type": "array",
                "items": { "oneOf": [{ "type": "integer" }, { "type": "string" }] }
            })
        );
    }

    #[test]
    fn schema_object_all_keys_required() {
        let schema = to_json_schema(&json!({ "a": 1, "b": "x" }));
        assert_eq!(
            schema,
            json!({
                "type": "object",
                "properties": { "a": { "type": "integer" }, "b": { "type": "string" } },
                "required": ["a", "b"]
            })
        );
    }

    #[test]
    fn schema_array_of_objects_merges_and_requires_common_keys() {
        let schema = to_json_schema(&json!([
            { "a": 1, "b": "x" },
            { "a": 2 }
        ]));
        let items = &schema["items"];
        assert_eq!(items["type"], "object");
        assert_eq!(items["properties"]["a"], json!({ "type": "integer" }));
        assert_eq!(items["properties"]["b"], json!({ "type": "string" }));
        assert_eq!(items["required"], json!(["a"]));
    }

    #[test]
    fn schema_nested_objects_merge_recursively() {
        let schema = to_json_schema(&json!([
            { "meta": { "x": 1 } },
            { "meta": { "x": 2, "y": true } }
        ]));
        let meta = &schema["items"]["properties"]["meta"];
        assert_eq!(meta["properties"]["x"], json!({ "type": "integer" }));
        assert_eq!(meta["properties"]["y"], json!({ "type": "boolean" }));
        assert_eq!(meta["required"], json!(["x"]));
    }

    #[test]
    fn schema_mixed_array_with_objects_appends_object_schema_last() {
        let schema = to_json_schema(&json!([1, { "a": 1 }]));
        let one_of = schema["items"]["oneOf"].as_array().unwrap();
        assert_eq!(one_of[0], json!({ "type": "integer" }));
        assert_eq!(one_of[1]["type"], "object");
    }

    // ── ToolResultStore ─────────────────────────────────────────────────

    #[tokio::test]
    async fn persist_plain_text_writes_content_txt() {
        let dir = tempfile::tempdir().unwrap();
        let store = ToolResultStore::new(dir.path().to_path_buf());

        let persisted = store
            .persist("chat:main", "call_1", "not json at all")
            .await
            .unwrap();

        assert!(persisted.content_path.ends_with("content.txt"));
        assert!(persisted.schema_path.is_none());
        assert_eq!(persisted.content_bytes, "not json at all".len());
        let on_disk = std::fs::read_to_string(&persisted.content_path).unwrap();
        assert_eq!(on_disk, "not json at all");
        // Path layout: <sessions>/tool-results/<session>/<call>/content.txt
        let expected = dir
            .path()
            .join(TOOL_RESULTS_DIR_NAME)
            .join("chat_main")
            .join("call_1")
            .join("content.txt");
        assert_eq!(persisted.content_path, expected);
    }

    #[tokio::test]
    async fn persist_json_writes_pretty_content_and_schema() {
        let dir = tempfile::tempdir().unwrap();
        let store = ToolResultStore::new(dir.path().to_path_buf());

        let raw = r#"{"result":{"stdout":"hello","exit_code":0}}"#;
        let persisted = store.persist("chat:main", "call_2", raw).await.unwrap();

        assert!(persisted.content_path.ends_with("content.json"));
        let schema_path = persisted.schema_path.as_ref().unwrap();
        assert!(schema_path.ends_with("schema.json"));

        let content = std::fs::read_to_string(&persisted.content_path).unwrap();
        // Pretty-printed (multi-line) and parseable back to the same value.
        assert!(content.contains('\n'));
        let round_trip: Value = serde_json::from_str(&content).unwrap();
        assert_eq!(round_trip, serde_json::from_str::<Value>(raw).unwrap());

        let schema: Value =
            serde_json::from_str(&std::fs::read_to_string(schema_path).unwrap()).unwrap();
        assert_eq!(schema["type"], "object");
        assert_eq!(
            schema["properties"]["result"]["properties"]["stdout"],
            json!({ "type": "string" })
        );
    }

    #[tokio::test]
    async fn persist_json_string_writes_plain_text() {
        let dir = tempfile::tempdir().unwrap();
        let store = ToolResultStore::new(dir.path().to_path_buf());

        let persisted = store
            .persist("s", "c", "\"just a text payload\"")
            .await
            .unwrap();

        assert!(persisted.content_path.ends_with("content.txt"));
        assert!(persisted.schema_path.is_none());
        let on_disk = std::fs::read_to_string(&persisted.content_path).unwrap();
        assert_eq!(on_disk, "just a text payload");
    }

    #[tokio::test]
    async fn persist_text_keeps_json_looking_text_in_content_txt() {
        let dir = tempfile::tempdir().unwrap();
        let store = ToolResultStore::new(dir.path().to_path_buf());

        let persisted = store
            .persist_text("s", "c", "{\n  \"line\": 1\n}")
            .await
            .unwrap();

        assert!(persisted.content_path.ends_with("content.txt"));
        assert!(persisted.schema_path.is_none());
        assert_eq!(
            std::fs::read_to_string(&persisted.content_path).unwrap(),
            "{\n  \"line\": 1\n}"
        );
    }

    #[tokio::test]
    async fn persist_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = ToolResultStore::new(dir.path().to_path_buf());

        let first = store.persist("s", "c", "first").await.unwrap();
        let second = store.persist("s", "c", "second").await.unwrap();

        assert_eq!(first.content_path, second.content_path);
        let on_disk = std::fs::read_to_string(&first.content_path).unwrap();
        assert_eq!(on_disk, "first", "existing files must be left untouched");
    }

    #[tokio::test]
    async fn persist_sanitizes_session_and_call_components() {
        let dir = tempfile::tempdir().unwrap();
        let store = ToolResultStore::new(dir.path().to_path_buf());

        let persisted = store
            .persist("../escape:attempt", "call/../1", "data")
            .await
            .unwrap();

        let expected = dir
            .path()
            .join(TOOL_RESULTS_DIR_NAME)
            .join(".._escape_attempt")
            .join("call_.._1")
            .join("content.txt");
        assert_eq!(persisted.content_path, expected);
    }

    #[tokio::test]
    async fn session_store_clear_removes_tool_results_dir() {
        let dir = tempfile::tempdir().unwrap();
        let session_store = crate::store::SessionStore::new(dir.path().to_path_buf());
        let store = ToolResultStore::new(dir.path().to_path_buf());

        session_store
            .append("chat:main", &json!({ "role": "user", "content": "hi" }))
            .await
            .unwrap();
        store.persist("chat:main", "call_1", "data").await.unwrap();
        let session_dir = store.session_dir("chat:main");
        assert!(session_dir.exists());

        session_store.clear("chat:main").await.unwrap();
        assert!(!session_dir.exists());
    }
}
