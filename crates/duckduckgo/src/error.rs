//! Error type for DuckDuckGo search.

/// Failure raised by the DuckDuckGo client or tool.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    /// A failure whose text is exposed verbatim to the agent.
    #[error("{message}")]
    Message { message: String },
    #[error(transparent)]
    Request(#[from] reqwest::Error),
    #[error(transparent)]
    UrlParse(#[from] url::ParseError),
    #[error(transparent)]
    Regex(#[from] regex::Error),
}

impl Error {
    #[must_use]
    pub(crate) fn message(message: impl Into<String>) -> Self {
        Self::Message {
            message: message.into(),
        }
    }
}

pub(crate) type Result<T> = std::result::Result<T, Error>;
