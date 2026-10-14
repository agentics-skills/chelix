use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Yaml(#[from] serde_yaml::Error),
    #[cfg(feature = "file-watcher")]
    #[error(transparent)]
    Notify(#[from] notify_debouncer_full::notify::Error),
    #[error("{0}")]
    Parse(String),
    #[error("{0}")]
    NotFound(String),
    #[error("skill file already exists: {0}")]
    AlreadyExists(String),
    #[error("{0}")]
    Validation(String),
}

pub type Result<T> = std::result::Result<T, Error>;
