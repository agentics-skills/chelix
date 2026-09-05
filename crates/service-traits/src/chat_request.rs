//! Closed request and internal context types for chat turn execution.

use {
    chelix_channels::{ChannelMessageKind, ChannelMessageMeta, ChannelReplyTarget, ChannelType},
    chelix_common::{MessageMedium, ModelOverride, ToolPolicy},
    chelix_config::schema::ToolChoice,
    chelix_sessions::{QueuedPromptContentBlock, SessionKey},
    serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _},
};

/// User message accepted by `chat.send`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatSendMessage {
    Text(String),
    Content(Vec<QueuedPromptContentBlock>),
}

/// Closed document metadata accepted by `chat.send`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatSendDocument {
    pub display_name: String,
    pub stored_filename: String,
    pub mime_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
}

/// Closed public request accepted by `chat.send`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatSendRequest {
    pub message: ChatSendMessage,
    pub model_override: Option<ModelOverride>,
    pub tool_choice: Option<ToolChoice>,
    pub documents: Vec<ChatSendDocument>,
    pub audio_filename: Option<String>,
    pub input_medium: Option<MessageMedium>,
    pub client_sequence: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChatSendRequestWire {
    text: Option<String>,
    content: Option<Vec<QueuedPromptContentBlock>>,
    model_override: Option<ModelOverride>,
    tool_choice: Option<ToolChoice>,
    #[serde(default)]
    documents: Vec<ChatSendDocument>,
    audio_filename: Option<String>,
    input_medium: Option<MessageMedium>,
    client_sequence: Option<u64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatSendRequestRef<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<&'a [QueuedPromptContentBlock]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model_override: Option<&'a ModelOverride>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'a ToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    documents: Option<&'a [ChatSendDocument]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    audio_filename: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input_medium: Option<MessageMedium>,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_sequence: Option<u64>,
}

impl<'de> Deserialize<'de> for ChatSendRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ChatSendRequestWire::deserialize(deserializer)?;
        let message = match (wire.text, wire.content) {
            (Some(text), None) => ChatSendMessage::Text(text),
            (None, Some(content)) => ChatSendMessage::Content(content),
            (None, None) => {
                return Err(D::Error::custom(
                    "exactly one of text or content is required",
                ));
            },
            (Some(_), Some(_)) => {
                return Err(D::Error::custom("text and content are mutually exclusive"));
            },
        };
        Ok(Self {
            message,
            model_override: wire.model_override,
            tool_choice: wire.tool_choice,
            documents: wire.documents,
            audio_filename: wire.audio_filename,
            input_medium: wire.input_medium,
            client_sequence: wire.client_sequence,
        })
    }
}

impl Serialize for ChatSendRequest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let (text, content) = match &self.message {
            ChatSendMessage::Text(text) => (Some(text.as_str()), None),
            ChatSendMessage::Content(content) => (None, Some(content.as_slice())),
        };
        ChatSendRequestRef {
            text,
            content,
            model_override: self.model_override.as_ref(),
            tool_choice: self.tool_choice.as_ref(),
            documents: (!self.documents.is_empty()).then_some(self.documents.as_slice()),
            audio_filename: self.audio_filename.as_deref(),
            input_medium: self.input_medium,
            client_sequence: self.client_sequence,
        }
        .serialize(serializer)
    }
}

impl ChatSendRequest {
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            message: ChatSendMessage::Text(text.into()),
            model_override: None,
            tool_choice: None,
            documents: Vec::new(),
            audio_filename: None,
            input_medium: None,
            client_sequence: None,
        }
    }
}

/// Closed public request accepted by `chat.send_sync`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatSendSyncRequest {
    pub text: String,
    pub model_override: Option<ModelOverride>,
    pub tool_choice: Option<ToolChoice>,
    pub input_medium: Option<MessageMedium>,
}

impl ChatSendSyncRequest {
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            model_override: None,
            tool_choice: None,
            input_medium: None,
        }
    }
}

/// Closed public request accepted by `chat.compact`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatCompactRequest {}

/// Closed public request accepted by `chat.context`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatContextRequest {}

/// Closed public request accepted by `chat.raw_prompt`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatRawPromptRequest {}

/// Closed public request accepted by `chat.full_context`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatFullContextRequest {}

/// Channel metadata retained with a user message and runtime context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatChannelMetadata {
    pub channel_type: ChannelType,
    pub sender_name: Option<String>,
    pub username: Option<String>,
    pub sender_id: Option<String>,
    pub message_kind: Option<ChannelMessageKind>,
}

impl From<ChannelMessageMeta> for ChatChannelMetadata {
    fn from(value: ChannelMessageMeta) -> Self {
        Self {
            channel_type: value.channel_type,
            sender_name: value.sender_name,
            username: value.username,
            sender_id: value.sender_id,
            message_kind: value.message_kind,
        }
    }
}

/// Origin of an internal chat execution request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatRequestOrigin {
    Client { connection_id: String },
    Internal,
}

/// Internal transport and execution context that is never deserialized from user JSON.
#[derive(Debug, Clone)]
pub struct ChatExecutionContext {
    pub session_id: SessionKey,
    pub origin: ChatRequestOrigin,
    pub accept_language: Option<String>,
    pub remote_ip: Option<String>,
    pub timezone: Option<String>,
    pub channel: Option<ChatChannelMetadata>,
    pub channel_reply_target: Option<ChannelReplyTarget>,
    pub tool_policy: Option<ToolPolicy>,
    pub agent_id: Option<String>,
}

impl ChatExecutionContext {
    #[must_use]
    pub fn internal(session_id: SessionKey) -> Self {
        Self {
            session_id,
            origin: ChatRequestOrigin::Internal,
            accept_language: None,
            remote_ip: None,
            timezone: None,
            channel: None,
            channel_reply_target: None,
            tool_policy: None,
            agent_id: None,
        }
    }

    #[must_use]
    pub fn client(session_id: SessionKey, connection_id: impl Into<String>) -> Self {
        Self {
            session_id,
            origin: ChatRequestOrigin::Client {
                connection_id: connection_id.into(),
            },
            accept_language: None,
            remote_ip: None,
            timezone: None,
            channel: None,
            channel_reply_target: None,
            tool_policy: None,
            agent_id: None,
        }
    }

    #[must_use]
    pub fn connection_id(&self) -> Option<&str> {
        match &self.origin {
            ChatRequestOrigin::Client { connection_id } => Some(connection_id),
            ChatRequestOrigin::Internal => None,
        }
    }
}
