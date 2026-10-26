use super::{
    ChelixConfig, DiagnosticCategory, Severity,
    schema_map::{KnownKeys, build_schema_map},
    validate_toml_str,
};

#[test]
fn diagnostic_category_display_matches_tracing_labels() {
    let cases = [
        (DiagnosticCategory::Agents, "agents"),
        (DiagnosticCategory::FileRef, "file-ref"),
        (DiagnosticCategory::InvalidValue, "invalid-value"),
        (DiagnosticCategory::MissingField, "missing-field"),
        (DiagnosticCategory::Security, "security"),
        (DiagnosticCategory::Syntax, "syntax"),
        (DiagnosticCategory::TypeError, "type-error"),
        (DiagnosticCategory::UnknownField, "unknown-field"),
        (DiagnosticCategory::UnknownProvider, "unknown-provider"),
        (DiagnosticCategory::ValidationWorker, "validation-worker"),
    ];
    for (category, label) in cases {
        assert_eq!(category.to_string(), label);
    }
}

#[path = "tests/agents.rs"]
mod agents;
#[path = "tests/auxiliary.rs"]
mod auxiliary;
#[path = "tests/channels.rs"]
mod channels;
#[path = "tests/common.rs"]
mod common;
#[path = "tests/memory.rs"]
mod memory;
#[path = "tests/providers.rs"]
mod providers;
#[path = "tests/security.rs"]
mod security;
#[path = "tests/structural.rs"]
mod structural;
#[path = "tests/tools.rs"]
mod tools;
