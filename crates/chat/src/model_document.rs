//! Enabled flag edits for one configured provider model.

use toml_edit::DocumentMut;

pub(crate) fn set_model_enabled(
    document: &mut DocumentMut,
    provider: &str,
    model_id: &str,
    enabled: bool,
) -> chelix_config::Result<()> {
    let Some(providers) = document
        .get_mut("providers")
        .and_then(toml_edit::Item::as_table_mut)
    else {
        return Err(chelix_config::Error::message(format!(
            "provider '{provider}' is not configured"
        )));
    };
    let Some(provider_item) = providers.get_mut(provider) else {
        return Err(chelix_config::Error::message(format!(
            "provider '{provider}' is not configured"
        )));
    };
    let Some(provider_table) = provider_item.as_table_mut() else {
        return Err(chelix_config::Error::message(format!(
            "providers.{provider} is not a table"
        )));
    };
    let Some(models_item) = provider_table.get_mut("models") else {
        return Err(chelix_config::Error::message(format!(
            "model '{model_id}' is not configured"
        )));
    };
    let Some(models) = models_item.as_table_mut() else {
        return Err(chelix_config::Error::message(format!(
            "providers.{provider}.models is not a table"
        )));
    };
    let Some(model_item) = models.get_mut(model_id) else {
        return Err(chelix_config::Error::message(format!(
            "model '{model_id}' is not configured"
        )));
    };
    let Some(model) = model_item.as_table_mut() else {
        return Err(chelix_config::Error::message(format!(
            "model '{model_id}' is not a table"
        )));
    };
    model.insert("enabled", toml_edit::value(enabled));
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn set_model_enabled_updates_only_the_requested_model() {
        let mut document = r#"
[providers.local-ai.models.alpha]
enabled = true

[providers.local-ai.models.beta]
enabled = true
"#
        .parse::<DocumentMut>()
        .expect("document");

        set_model_enabled(&mut document, "local-ai", "beta", false).expect("disable model");

        assert_eq!(
            document["providers"]["local-ai"]["models"]["alpha"]["enabled"].as_bool(),
            Some(true)
        );
        assert_eq!(
            document["providers"]["local-ai"]["models"]["beta"]["enabled"].as_bool(),
            Some(false)
        );
    }

    #[test]
    fn set_model_enabled_rejects_a_missing_model_without_creating_tables() {
        let mut document = DocumentMut::new();
        let error = set_model_enabled(&mut document, "local-ai", "beta", false)
            .expect_err("missing provider");
        assert!(
            error
                .to_string()
                .contains("provider 'local-ai' is not configured")
        );
        assert!(document.get("providers").is_none());
    }
}
