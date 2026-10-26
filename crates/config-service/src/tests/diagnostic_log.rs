use std::{path::Path, sync::Arc};

use chelix_config::{ChelixConfig, Diagnostic, DiagnosticCategory, Severity};

use crate::{
    ConfigService, ConfigValidation, ConfigValidator, SyncValidator, validation::ValidationScope,
};

#[tokio::test]
async fn replacement_errors_preserve_safe_category_and_position()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("chelix.toml");
    tokio::fs::write(&path, "[server]\nport = 12000\n").await?;
    let service = ConfigService::open_at(
        path,
        ConfigValidation::default(),
        crate::ConfigInitialization::default(),
    )
    .await?;
    let cases = [
        (
            "[providers.custom]\napi_key = \"fixture-private\" extra\n",
            "fixture-private",
            "at line 2, column ",
        ),
        (
            "[providers.custom]\napi_key = 12345678\n",
            "12345678",
            "invalid configuration field type at line 1, column 2",
        ),
        (
            "[env]\nTOKEN = \"fixture-private\"\n[providers.custom]\napi_key = \"${TOKEN}\"\n",
            "fixture-private",
            "invalid configuration after environment substitution",
        ),
    ];
    for (raw, secret, expected) in cases {
        let Err(error) = service.replace_document(raw.into(), None).await else {
            panic!("expected replacement rejection");
        };
        let message = error.to_string();
        assert!(message.contains(expected), "{message}");
        assert!(!message.contains(secret), "{message}");
    }
    Ok(())
}

fn summarized_rejection(_: &ChelixConfig) -> Vec<Diagnostic> {
    vec![Diagnostic {
        severity: Severity::Error,
        category: DiagnosticCategory::InvalidValue,
        path: "server.port".into(),
        message: "owner detail for the configuration editor".into(),
        summary_message: Some("owner rejection summary".into()),
    }]
}

#[tokio::test]
async fn owner_summary_is_used_for_transaction_errors() {
    let checks: Vec<Arc<dyn ConfigValidator>> = vec![Arc::new(SyncValidator(summarized_rejection))];
    let validation = ConfigValidation::new(checks.clone(), checks);
    let raw = "[server]\nport = 12000\n";
    let path = Path::new("chelix.toml");
    let diagnosed = validation.diagnose(raw, path).await;
    assert_eq!(
        diagnosed.diagnostics[0].message,
        "owner detail for the configuration editor"
    );
    let Err(error) = validation
        .admit(raw.into(), path, ValidationScope::Full, None)
        .await
    else {
        panic!("expected owner rejection");
    };
    assert_eq!(
        error.to_string(),
        "configuration rejected: server.port: owner rejection summary"
    );
}
