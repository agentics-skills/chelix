//! Strict decoding of tool-call arguments.
//!
//! Provider argument text is parsed exactly once, after the whole string has
//! arrived. Text that is not a JSON object is never repaired: the decode
//! failure is carried as a diagnostic so the runner refuses the call and
//! returns the parser error to the model.

use super::types::{ToolCallArgumentDiagnostic, ToolCallArgumentSource};

const NOT_AN_OBJECT_ERROR: &str = "tool arguments must be a JSON object";

/// Decode tool-call arguments from provider or persisted JSON.
///
/// OpenAI-style APIs typically encode `arguments` as a JSON string, while some
/// compatible backends return native JSON directly. Preserve the native shape
/// when it is already structured and only parse when the payload is a string.
#[must_use]
pub fn decode_tool_call_arguments(arguments: Option<&serde_json::Value>) -> serde_json::Value {
    decode_tool_call_arguments_with_diagnostic(arguments).arguments
}

#[derive(Debug, Clone)]
pub struct DecodedToolCallArguments {
    pub arguments: serde_json::Value,
    pub diagnostic: Option<ToolCallArgumentDiagnostic>,
}

/// Decode tool-call arguments while preserving raw-provider diagnostics.
///
/// Strings that fail to decode still yield `{}` so the call keeps a
/// well-formed shape for lifecycle events, but the diagnostic marks them as
/// malformed and the runner rejects the call before execution.
#[must_use]
pub fn decode_tool_call_arguments_with_diagnostic(
    arguments: Option<&serde_json::Value>,
) -> DecodedToolCallArguments {
    match arguments {
        Some(serde_json::Value::String(raw)) => decode_tool_call_arguments_from_str(raw),
        Some(serde_json::Value::Null) | None => DecodedToolCallArguments {
            arguments: serde_json::Value::Object(Default::default()),
            diagnostic: Some(ToolCallArgumentDiagnostic {
                source: ToolCallArgumentSource::NullOrMissing,
                raw_len: None,
                raw_preview: None,
                parse_error: None,
            }),
        },
        Some(value) => DecodedToolCallArguments {
            arguments: value.clone(),
            diagnostic: None,
        },
    }
}

/// Decode an OpenAI-style function-call argument string.
///
/// An empty string is the provider convention for a call without parameters
/// and decodes to `{}`. Any other text must be a JSON object; a parse error or
/// a non-object value is reported as malformed with the exact parser message.
#[must_use]
pub fn decode_tool_call_arguments_from_str(raw: &str) -> DecodedToolCallArguments {
    if raw.trim().is_empty() {
        return DecodedToolCallArguments {
            arguments: serde_json::Value::Object(Default::default()),
            diagnostic: Some(ToolCallArgumentDiagnostic {
                source: ToolCallArgumentSource::EmptyString,
                raw_len: Some(raw.len()),
                raw_preview: Some(raw_argument_preview(raw)),
                parse_error: None,
            }),
        };
    }

    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(arguments @ serde_json::Value::Object(_)) => DecodedToolCallArguments {
            arguments,
            diagnostic: None,
        },
        Ok(_) => malformed(raw, NOT_AN_OBJECT_ERROR.to_string()),
        Err(error) => malformed(raw, error.to_string()),
    }
}

fn malformed(raw: &str, parse_error: String) -> DecodedToolCallArguments {
    DecodedToolCallArguments {
        arguments: serde_json::Value::Object(Default::default()),
        diagnostic: Some(ToolCallArgumentDiagnostic {
            source: ToolCallArgumentSource::MalformedString,
            raw_len: Some(raw.len()),
            raw_preview: Some(raw_argument_preview(raw)),
            parse_error: Some(parse_error),
        }),
    }
}

fn raw_argument_preview(raw: &str) -> String {
    const MAX_PREVIEW_CHARS: usize = 160;
    let mut preview: String = raw.chars().take(MAX_PREVIEW_CHARS).collect();
    if raw.chars().count() > MAX_PREVIEW_CHARS {
        preview.push_str("...");
    }
    preview
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_tool_call_arguments_parses_json_string() {
        let arguments = serde_json::json!("{\"cmd\":\"ls\"}");

        let decoded = decode_tool_call_arguments(Some(&arguments));

        assert_eq!(decoded, serde_json::json!({"cmd": "ls"}));
    }

    #[test]
    fn decode_tool_call_arguments_preserves_native_json() {
        let arguments = serde_json::json!({"cmd": "ls"});

        let decoded = decode_tool_call_arguments(Some(&arguments));

        assert_eq!(decoded, arguments);
    }

    #[test]
    fn decode_tool_call_arguments_preserves_serializer_escapes() {
        let raw = serde_json::json!({"command": "echo \"hi\" \\ path\nline2"}).to_string();

        let decoded = decode_tool_call_arguments_from_str(&raw);

        assert!(decoded.diagnostic.is_none());
        assert_eq!(decoded.arguments["command"], "echo \"hi\" \\ path\nline2");
    }

    #[test]
    fn decode_tool_call_arguments_rejects_truncated_json_string() {
        let decoded = decode_tool_call_arguments_from_str(r#"{"command":"git status""#);

        assert_eq!(decoded.arguments, serde_json::json!({}));
        let diagnostic = decoded.diagnostic.unwrap();
        assert_eq!(diagnostic.source, ToolCallArgumentSource::MalformedString);
        assert!(diagnostic.parse_error.is_some());
    }

    #[test]
    fn decode_tool_call_arguments_rejects_invalid_escape() {
        let decoded = decode_tool_call_arguments_from_str(r#"{"pattern":"sessions\.patch"}"#);

        assert_eq!(decoded.arguments, serde_json::json!({}));
        let diagnostic = decoded.diagnostic.unwrap();
        assert_eq!(diagnostic.source, ToolCallArgumentSource::MalformedString);
        assert!(diagnostic.parse_error.unwrap().contains("escape"));
    }

    #[test]
    fn decode_tool_call_arguments_rejects_raw_newline_inside_string() {
        let decoded = decode_tool_call_arguments_from_str("{\"content\":\"a\nb\"}");

        assert_eq!(decoded.arguments, serde_json::json!({}));
        let diagnostic = decoded.diagnostic.unwrap();
        assert_eq!(diagnostic.source, ToolCallArgumentSource::MalformedString);
        assert!(
            diagnostic
                .parse_error
                .unwrap()
                .contains("control character")
        );
    }

    #[test]
    fn decode_tool_call_arguments_rejects_non_object_json() {
        let decoded = decode_tool_call_arguments_from_str("[1]");

        assert_eq!(decoded.arguments, serde_json::json!({}));
        let diagnostic = decoded.diagnostic.unwrap();
        assert_eq!(diagnostic.source, ToolCallArgumentSource::MalformedString);
        assert_eq!(diagnostic.parse_error.as_deref(), Some(NOT_AN_OBJECT_ERROR));
    }

    #[test]
    fn decode_tool_call_arguments_preserves_empty_string_diagnostic() {
        let decoded = decode_tool_call_arguments_from_str("");

        assert_eq!(decoded.arguments, serde_json::json!({}));
        let diagnostic = decoded.diagnostic.unwrap();
        assert_eq!(diagnostic.source, ToolCallArgumentSource::EmptyString);
        assert_eq!(diagnostic.raw_len, Some(0));
    }

    #[test]
    fn decode_tool_call_arguments_preserves_unrecoverable_string_diagnostic() {
        let decoded = decode_tool_call_arguments_from_str("not json at all");

        assert_eq!(decoded.arguments, serde_json::json!({}));
        let diagnostic = decoded.diagnostic.unwrap();
        assert_eq!(diagnostic.source, ToolCallArgumentSource::MalformedString);
        assert!(diagnostic.parse_error.is_some());
        assert_eq!(diagnostic.raw_preview.as_deref(), Some("not json at all"));
    }
}
