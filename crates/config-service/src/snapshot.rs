use std::{fmt, path::PathBuf, sync::Arc};

use chelix_config::ChelixConfig;

pub(crate) struct Snapshot {
    pub(crate) raw: String,
    pub(crate) config: Arc<ChelixConfig>,
}

/// The stored TOML and its display location returned by a queued read.
pub struct ConfigDocument {
    pub toml: String,
    pub path: PathBuf,
}

impl fmt::Debug for ConfigDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfigDocument")
            .field("path", &self.path)
            .field("toml_bytes", &self.toml.len())
            .finish_non_exhaustive()
    }
}
