use super::*;

#[test]
fn auxiliary_title_pair_accepts_complete_configuration() {
    for effort in ["low", "off"] {
        let input = format!(
            "[auxiliary.title_generation]\nmodel = \"test::title\"\nreasoning_effort = \"{effort}\"\n"
        );
        let result = validate_toml_str(&input);
        assert!(!result.has_errors(), "{:?}", result.diagnostics);

        let config: ChelixConfig = toml::from_str(&input).unwrap();
        let pair = config.auxiliary.title_generation.unwrap();
        assert_eq!(pair.model, "test::title");
        assert_eq!(pair.reasoning_effort.as_str(), effort);
    }
}

#[test]
fn auxiliary_title_pair_requires_both_fields() {
    for (fields, missing_field) in [
        ("", "model"),
        ("reasoning_effort = \"low\"", "model"),
        ("model = \"test::title\"", "reasoning_effort"),
    ] {
        let input = format!("[auxiliary.title_generation]\n{fields}\n");
        let result = validate_toml_str(&input);
        assert!(
            result.diagnostics.iter().any(|diagnostic| {
                diagnostic.severity == Severity::Error
                    && diagnostic.category == "type-error"
                    && diagnostic.message.contains(missing_field)
            }),
            "{:?}",
            result.diagnostics
        );
        let error = toml::from_str::<ChelixConfig>(&input).unwrap_err();
        assert!(error.to_string().contains(missing_field), "{error}");
    }
}

#[test]
fn auxiliary_rejects_arbitrary_extra_key_at_each_level() {
    for (input, path) in [
        (
            "[auxiliary]\narbitrary_extra_key = true\n[auxiliary.title_generation]\nmodel = \"test::title\"\nreasoning_effort = \"low\"\n",
            "auxiliary.arbitrary_extra_key",
        ),
        (
            "[auxiliary.title_generation]\nmodel = \"test::title\"\nreasoning_effort = \"low\"\narbitrary_extra_key = true\n",
            "auxiliary.title_generation.arbitrary_extra_key",
        ),
    ] {
        let result = validate_toml_str(input);
        assert!(
            result.diagnostics.iter().any(|diagnostic| {
                diagnostic.severity == Severity::Error
                    && diagnostic.category == "unknown-field"
                    && diagnostic.path == path
            }),
            "{:?}",
            result.diagnostics
        );
        let error = toml::from_str::<ChelixConfig>(input).unwrap_err();
        assert!(error.to_string().contains("arbitrary_extra_key"), "{error}");
    }
}
