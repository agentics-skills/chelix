use super::*;

#[test]
fn plaintext_provider_api_key_warned() {
    let toml = r#"
[providers.openai]
api_key = "sk-real-key-here"
"#;
    let result = validate_toml_str(toml);
    let warning = result.diagnostics.iter().find(|d| {
        d.category == DiagnosticCategory::Security && d.path == "providers.openai.api_key"
    });
    assert!(
        warning.is_none(),
        "provider api_key in chelix.toml is stored configuration, got: {:?}",
        result.diagnostics
    );
}

#[test]
fn env_var_provider_api_key_not_warned() {
    let toml = r#"
[providers.openai]
api_key = "${OPENAI_API_KEY}"
"#;
    let result = validate_toml_str(toml);
    let warning = result.diagnostics.iter().find(|d| {
        d.category == DiagnosticCategory::Security && d.path == "providers.openai.api_key"
    });
    assert!(
        warning.is_none(),
        "env var substitution should not trigger plaintext warning, got: {:?}",
        result.diagnostics
    );
}

#[test]
fn env_var_no_braces_not_warned() {
    let toml = r#"
[providers.openai]
api_key = "$OPENAI_API_KEY"
"#;
    let result = validate_toml_str(toml);
    let warning = result.diagnostics.iter().find(|d| {
        d.category == DiagnosticCategory::Security && d.path == "providers.openai.api_key"
    });
    assert!(
        warning.is_none(),
        "$VAR syntax should not trigger plaintext warning, got: {:?}",
        result.diagnostics
    );
}

#[test]
fn plaintext_voice_api_key_not_warned() {
    let toml = r#"
[voice.tts.elevenlabs]
api_key = "el-real-key"
"#;
    let result = validate_toml_str(toml);
    let warning = result.diagnostics.iter().find(|d| {
        d.category == DiagnosticCategory::Security && d.path == "voice.tts.elevenlabs.api_key"
    });
    assert!(
        warning.is_none(),
        "voice api_key in chelix.toml is stored configuration, got: {:?}",
        result.diagnostics
    );
}
