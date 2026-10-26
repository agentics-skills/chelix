//! Shared operations on TOML tables.

use toml_edit::{Item, Table};

/// Get a nested table, creating it with the requested layout.
pub fn nested_table<'a>(
    parent: &'a mut Table,
    key: &str,
    implicit: bool,
) -> crate::Result<&'a mut Table> {
    if !parent.contains_key(key) {
        let mut table = Table::new();
        table.set_implicit(implicit);
        parent.insert(key, Item::Table(table));
    }
    parent
        .get_mut(key)
        .and_then(Item::as_table_mut)
        .ok_or_else(|| crate::Error::message(format!("`{key}` is not a TOML table")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_table_preserves_layout_and_existing_values() -> crate::Result<()> {
        for implicit in [false, true] {
            let mut parent = Table::new();
            let created = nested_table(&mut parent, "providers", implicit)?;
            assert_eq!(created.is_implicit(), implicit);
            created.insert("enabled", toml_edit::value(true));

            let existing = nested_table(&mut parent, "providers", !implicit)?;
            assert_eq!(existing.is_implicit(), implicit);
            assert_eq!(existing.get("enabled").and_then(Item::as_bool), Some(true));

            parent.insert("server", toml_edit::value(1));
            assert!(nested_table(&mut parent, "server", false).is_err());
            assert_eq!(parent.get("server").and_then(Item::as_integer), Some(1));
        }
        Ok(())
    }
}
