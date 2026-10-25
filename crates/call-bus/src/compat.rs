//! Load-time compatibility checks.

use semver::{Version, VersionReq};

use crate::document::PluginDocument;

pub fn version_matches(version: &Version, requirement: &str) -> bool {
    VersionReq::parse(requirement)
        .map(|requirement| requirement.matches(version))
        .unwrap_or(false)
}

pub fn protocol_ok(document: &PluginDocument) -> Result<(), String> {
    if document.protocol == 1 {
        Ok(())
    } else {
        Err(format!(
            "plugin {} protocol {} is not 1",
            document.name, document.protocol
        ))
    }
}
