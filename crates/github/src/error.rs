//! Error type for the GitHub tool set.

/// Failure raised by the GitHub client or one of the `github_*` tools.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A tool that requires authentication was called without a configured
    /// personal access token.
    #[error(
        "GitHub personal access token is not configured: set `tools.github.pat` in chelix.toml"
    )]
    MissingToken,
    /// GitHub rejected the request with `401 Unauthorized` or `403 Forbidden`
    /// while a personal access token was sent.
    #[error("{message}")]
    Authorization { message: String },
    /// Any other failure reported with the message the tool exposes verbatim.
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
