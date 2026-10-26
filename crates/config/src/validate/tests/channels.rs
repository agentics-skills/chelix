use super::*;

#[test]
fn channels_unknown_account_type_rejected() {
    let result = validate_toml_str("[channels.unknown_type.bot]\ntoken = 'test'");
    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "channels.unknown_type" && diagnostic.severity == Severity::Error
    }));
}
