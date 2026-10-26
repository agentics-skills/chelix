use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to read configuration {}: {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to write configuration {}: {source}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Config(#[from] chelix_config::Error),
    #[error("configuration rejected: {0}")]
    Validation(String),
    #[error("configuration service queue closed")]
    QueueClosed,
    #[error("configuration worker failed: {0}")]
    Worker(String),
}

impl Error {
    #[must_use]
    pub fn is_validation(&self) -> bool {
        matches!(self, Self::Config(_) | Self::Validation(_))
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn admission_log_reason(raw: &str, path: &std::path::Path) -> Option<String> {
    if let Err(error) = raw.parse::<toml_edit::DocumentMut>() {
        return Some(format!(
            "{}{}",
            error.message(),
            log_position(error.span(), Some(raw))
        ));
    }
    if let Err(error) = toml::from_str::<chelix_config::ChelixConfig>(raw) {
        return Some(format!(
            "invalid configuration field type{}",
            log_position(error.span(), Some(raw))
        ));
    }
    chelix_config::parse_config_source(raw, path)
        .err()
        .map(|_| "invalid configuration after environment substitution".to_owned())
}

#[cfg(feature = "tracing")]
pub(crate) fn config_log_reason(error: &chelix_config::Error) -> String {
    use chelix_config::Error;

    match error {
        Error::Io(error) => error.to_string(),
        Error::Json(error) => json_log_reason(error),
        Error::TomlDe(error) => toml_type_log_reason(error),
        Error::TomlSer(_) => "configuration serialization failed".into(),
        Error::TomlEdit(error) => toml_syntax_log_reason(error),
        Error::Message { message } => message.clone(),
        Error::External { context, source } => {
            let reason = if let Some(error) = source.downcast_ref::<toml_edit::TomlError>() {
                toml_syntax_log_reason(error)
            } else if let Some(error) = source.downcast_ref::<toml::de::Error>() {
                toml_type_log_reason(error)
            } else if let Some(error) = source.downcast_ref::<serde_json::Error>() {
                json_log_reason(error)
            } else {
                "external configuration error".into()
            };
            format!("{context}: {reason}")
        },
    }
}

#[cfg(feature = "tracing")]
fn toml_syntax_log_reason(error: &toml_edit::TomlError) -> String {
    format!("{}{}", error.message(), log_position(error.span(), None))
}

#[cfg(feature = "tracing")]
fn toml_type_log_reason(error: &toml::de::Error) -> String {
    format!(
        "configuration parse/type rejection{}",
        log_position(error.span(), None)
    )
}

#[cfg(feature = "tracing")]
fn json_log_reason(error: &serde_json::Error) -> String {
    format!(
        "configuration deserialization failed ({:?}) at line {}, column {}",
        error.classify(),
        error.line(),
        error.column()
    )
}

fn log_position(span: Option<std::ops::Range<usize>>, raw: Option<&str>) -> String {
    let Some(span) = span else {
        return String::new();
    };
    let Some(raw) = raw else {
        return format!(" at bytes {}..{}", span.start, span.end);
    };
    let (line, column) = raw
        .char_indices()
        .take_while(|(offset, _)| *offset < span.start)
        .fold((1, 1), |(line, column), (_, character)| {
            if character == '\n' {
                (line + 1, 1)
            } else {
                (line, column + 1)
            }
        });
    format!(" at line {line}, column {column}")
}
