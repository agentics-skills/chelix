//! Voice and phone secret patches against the stored TOML document.
//!
//! Typed configuration changes use [`chelix_config_service::ConfigService::update`].
//! Secret strings are written on the document so they are not reserialized through
//! the typed config value.

use std::sync::Arc;

use chelix_config::{ChelixConfig, toml_document::nested_table};

pub(crate) async fn read_config(
    service: &chelix_config_service::ConfigService,
) -> chelix_config_service::Result<Arc<ChelixConfig>> {
    service.read(Arc::clone).await
}

pub(crate) fn owned_string_paths(
    updates: &[(&[&str], Option<&str>)],
) -> Vec<(Vec<String>, Option<String>)> {
    updates
        .iter()
        .map(|(path, value)| {
            (
                path.iter().map(|segment| (*segment).to_string()).collect(),
                value.map(str::to_string),
            )
        })
        .collect()
}

pub(crate) async fn edit_string_paths(
    service: &chelix_config_service::ConfigService,
    updates: Vec<(Vec<String>, Option<String>)>,
) -> chelix_config_service::Result<()> {
    service
        .edit_document(move |document| {
            let root = document.as_table_mut();
            for (path, value) in &updates {
                set_string_path(root, path, value.as_deref())?;
            }
            Ok(())
        })
        .await
}

fn set_string_path(
    root: &mut toml_edit::Table,
    path: &[String],
    value: Option<&str>,
) -> chelix_config::Result<()> {
    let [head, rest @ ..] = path else {
        return Err(chelix_config::Error::message("config path is empty"));
    };
    if rest.is_empty() {
        match value.map(str::trim).filter(|text| !text.is_empty()) {
            Some(text) => {
                root.insert(head, toml_edit::value(text));
            },
            None => {
                root.remove(head);
            },
        }
        return Ok(());
    }
    let child = nested_table(root, head, false)?;
    set_string_path(child, rest, value)
}

#[cfg(test)]
pub(crate) async fn open_test_service() -> chelix_config_service::ConfigService {
    chelix_config_service::ConfigService::open(
        crate::config_wiring::validators(),
        crate::config_wiring::initializers(),
    )
    .await
    .unwrap_or_else(|error| panic!("config service should open: {error}"))
}
