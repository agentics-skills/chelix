//! Plugin records, statuses, and provider listings.

use crate::document::PluginDocument;
use crate::info::{PluginInfo, PluginStatus};

#[derive(Debug)]
pub struct PluginRecord {
    pub document: PluginDocument,
    pub directory: std::path::PathBuf,
    pub status: PluginStatus,
    pub bootstrap: bool,
    pub loaded: bool,
}

impl PluginRecord {
    #[must_use]
    pub fn info(&self) -> PluginInfo {
        PluginInfo {
            name: self.document.name.clone(),
            api: Some(self.document.api.clone()),
            version: Some(self.document.version.clone()),
            status: self.status.clone(),
            document: Some(self.document.raw.clone()),
        }
    }
}
