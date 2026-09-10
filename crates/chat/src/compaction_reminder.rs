//! System-prompt reminder restored after context compaction.

use serde_json::Value;

use crate::error::{self, Error};

const REMINDER_OPEN: &str = "<REMINDER>";
const REMINDER_CLOSE: &str = "</REMINDER>";

#[derive(Debug, Clone)]
pub(crate) struct CompactionReminder {
    text: Option<String>,
    checkpoint_active: bool,
}

impl CompactionReminder {
    pub(crate) fn from_history(enabled: bool, history: &[Value]) -> error::Result<Self> {
        if !enabled {
            return Ok(Self {
                text: None,
                checkpoint_active: false,
            });
        }

        let checkpoint_active = history
            .iter()
            .any(|message| message.get("role").and_then(Value::as_str) == Some("checkpoint"));
        let text = history
            .iter()
            .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))
            .map(first_user_text)
            .transpose()?
            .flatten();

        Ok(Self {
            text,
            checkpoint_active,
        })
    }

    pub(crate) fn activate(&mut self) {
        self.checkpoint_active = true;
    }

    #[must_use]
    pub(crate) fn is_active(&self) -> bool {
        self.checkpoint_active && self.text.is_some()
    }

    #[must_use]
    pub(crate) fn active_segment(&self) -> Option<String> {
        let text = self.text.as_deref().filter(|_| self.checkpoint_active)?;
        Some(format!("\n\n{REMINDER_OPEN}\n{text}\n{REMINDER_CLOSE}"))
    }

    #[must_use]
    pub(crate) fn render(&self, base_system_prompt: &str) -> String {
        let Some(segment) = self.active_segment() else {
            return base_system_prompt.to_string();
        };
        format!("{base_system_prompt}{segment}")
    }
}

fn first_user_text(message: &Value) -> error::Result<Option<String>> {
    let content = message
        .get("content")
        .ok_or_else(|| Error::message("first persisted user message is missing content"))?;
    if let Some(text) = content.as_str() {
        return Ok(Some(text.to_string()));
    }

    let blocks = content.as_array().ok_or_else(|| {
        Error::message("first persisted user message content must be a string or array")
    })?;
    let mut text = String::new();
    let mut saw_text = false;
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                let block_text = block.get("text").and_then(Value::as_str).ok_or_else(|| {
                    Error::message("first persisted user message has an invalid text block")
                })?;
                text.push_str(block_text);
                saw_text = true;
            },
            Some("image_url") => {},
            Some(other) => {
                return Err(Error::message(format!(
                    "first persisted user message has unsupported content block type '{other}'"
                )));
            },
            None => {
                return Err(Error::message(
                    "first persisted user message has a content block without a valid type",
                ));
            },
        }
    }

    Ok(saw_text.then_some(text))
}

#[allow(clippy::unwrap_used)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_rendered_byte_for_byte_after_checkpoint() {
        let history = vec![
            serde_json::json!({"role": "user", "content": "  first\nmessage  "}),
            serde_json::json!({"role": "checkpoint", "summary": "summary"}),
        ];

        let reminder = CompactionReminder::from_history(true, &history).unwrap();

        assert_eq!(
            reminder.render("system"),
            "system\n\n<REMINDER>\n  first\nmessage  \n</REMINDER>"
        );
    }

    #[test]
    fn multimodal_text_blocks_are_concatenated_without_media_or_separators() {
        let history = vec![
            serde_json::json!({
                "role": "user",
                "content": [
                    {"type": "text", "text": "first"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,AA=="}},
                    {"type": "text", "text": "second"}
                ],
                "audio": "media/voice.webm",
                "documents": [{"display_name": "notes.txt"}]
            }),
            serde_json::json!({"role": "checkpoint", "summary": "summary"}),
        ];

        let reminder = CompactionReminder::from_history(true, &history).unwrap();

        assert_eq!(
            reminder.render("system"),
            "system\n\n<REMINDER>\nfirstsecond\n</REMINDER>"
        );
    }

    #[test]
    fn image_only_first_user_does_not_fall_through_to_later_user() {
        let history = vec![
            serde_json::json!({
                "role": "user",
                "content": [{"type": "image_url", "image_url": {"url": "data:image/png;base64,AA=="}}]
            }),
            serde_json::json!({"role": "user", "content": "later text"}),
            serde_json::json!({"role": "checkpoint", "summary": "summary"}),
        ];

        let reminder = CompactionReminder::from_history(true, &history).unwrap();

        assert!(!reminder.is_active());
        assert_eq!(reminder.render("system"), "system");
    }

    #[test]
    fn explicit_empty_text_renders_an_empty_reminder_body() {
        let history = vec![
            serde_json::json!({"role": "user", "content": ""}),
            serde_json::json!({"role": "checkpoint", "summary": "summary"}),
        ];

        let reminder = CompactionReminder::from_history(true, &history).unwrap();

        assert!(reminder.is_active());
        assert_eq!(
            reminder.render("system"),
            "system\n\n<REMINDER>\n\n</REMINDER>"
        );
    }

    #[test]
    fn reminder_activates_only_after_checkpoint_and_never_duplicates() {
        let history = vec![serde_json::json!({"role": "user", "content": "task"})];
        let mut reminder = CompactionReminder::from_history(true, &history).unwrap();

        assert_eq!(reminder.render("system"), "system");
        reminder.activate();
        let rendered = reminder.render("system");
        assert_eq!(rendered, "system\n\n<REMINDER>\ntask\n</REMINDER>");
        assert_eq!(reminder.render("system"), rendered);
    }

    #[test]
    fn missing_first_user_keeps_system_prompt_unchanged() {
        let history = vec![serde_json::json!({"role": "checkpoint", "summary": "summary"})];

        let reminder = CompactionReminder::from_history(true, &history).unwrap();

        assert_eq!(reminder.render("system"), "system");
    }

    #[test]
    fn malformed_first_user_content_is_an_error() {
        let history = vec![serde_json::json!({"role": "user", "content": 42})];

        let error = CompactionReminder::from_history(true, &history).unwrap_err();

        assert!(error.to_string().contains("must be a string or array"));
    }

    #[test]
    fn disabled_reminder_does_not_parse_user_content() {
        let history = vec![serde_json::json!({"role": "user", "content": 42})];

        let reminder = CompactionReminder::from_history(false, &history).unwrap();

        assert_eq!(reminder.render("system"), "system");
    }
}
