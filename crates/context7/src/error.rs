//! Error type for the Context7 tool set.

/// Failure raised by the Context7 client or one of the `context7_*` tools.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Context7 rejected the configured API token.
    #[error("{message}")]
    Authorization { message: String },
    /// Any other failure reported with the message exposed to the agent.
    #[error("{message}")]
    Message { message: String },
    #[error(transparent)]
    Request(#[from] reqwest::Error),
    #[error(transparent)]
    UrlParse(#[from] url::ParseError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl Error {
    #[must_use]
    pub fn message(message: impl Into<String>) -> Self {
        Self::Message {
            message: message.into(),
        }
    }

    #[must_use]
    pub fn authorization(message: impl Into<String>) -> Self {
        Self::Authorization {
            message: message.into(),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
