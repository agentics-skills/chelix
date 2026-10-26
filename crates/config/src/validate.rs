//! Structural configuration validation.
//!
//! Checks TOML syntax, deserializable types, and unknown fields against the
//! schema map. Business rules live in the crates that own those fields.

use crate::schema::ChelixConfig;

#[path = "validate/schema_map.rs"]
pub(crate) mod schema_map;

/// Severity level for a diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Error => write!(f, "error"),
            Self::Warning => write!(f, "warning"),
            Self::Info => write!(f, "info"),
        }
    }
}

/// Diagnostic class. `Display` is the label recorded by tracing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticCategory {
    Agents,
    FileRef,
    InvalidValue,
    MissingField,
    Security,
    Syntax,
    TypeError,
    UnknownField,
    UnknownProvider,
    ValidationWorker,
}

impl std::fmt::Display for DiagnosticCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Agents => "agents",
            Self::FileRef => "file-ref",
            Self::InvalidValue => "invalid-value",
            Self::MissingField => "missing-field",
            Self::Security => "security",
            Self::Syntax => "syntax",
            Self::TypeError => "type-error",
            Self::UnknownField => "unknown-field",
            Self::UnknownProvider => "unknown-provider",
            Self::ValidationWorker => "validation-worker",
        })
    }
}

/// A single validation diagnostic.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub severity: Severity,
    pub category: DiagnosticCategory,
    /// Dotted path, e.g. "server.bnd"
    pub path: String,
    /// Detailed diagnostic text.
    pub message: String,
    /// Text for transaction errors and operational logs.
    pub summary_message: Option<String>,
}

/// Result of validating a configuration file.
#[derive(Debug, Clone)]
pub struct ValidationResult {
    pub diagnostics: Vec<Diagnostic>,
    pub config_path: Option<std::path::PathBuf>,
}

impl ValidationResult {
    /// Returns `true` if any diagnostic is an error.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }

    /// Count diagnostics by severity.
    #[must_use]
    pub fn count(&self, severity: Severity) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == severity)
            .count()
    }
}

/// Validate TOML syntax, unknown fields, and deserializable types.
#[must_use]
pub fn validate_toml_str(toml_str: &str) -> ValidationResult {
    let mut diagnostics = Vec::new();

    // 1. Syntax - parse raw TOML
    let toml_value: toml::Value = match toml::from_str(toml_str) {
        Ok(v) => v,
        Err(e) => {
            diagnostics.push(Diagnostic {
                severity: Severity::Error,
                category: DiagnosticCategory::Syntax,
                path: String::new(),
                message: format!("TOML syntax error: {e}"),
                summary_message: None,
            });
            return ValidationResult {
                diagnostics,
                config_path: None,
            };
        },
    };

    // 2. Unknown fields - walk the TOML tree against KnownKeys
    let schema = schema_map::build_schema_map();
    match serde_json::to_value(&toml_value) {
        Ok(value) => schema_map::check_unknown_fields(&value, &schema, "", &mut diagnostics),
        Err(error) => diagnostics.push(Diagnostic {
            severity: Severity::Error,
            category: DiagnosticCategory::Syntax,
            path: String::new(),
            message: format!("failed to inspect TOML structure: {error}"),
            summary_message: None,
        }),
    }

    // 3. Type check. A successful parse has no further structural diagnostics.
    if let Err(error) = toml::from_str::<ChelixConfig>(toml_str) {
        diagnostics.push(Diagnostic {
            severity: Severity::Error,
            category: DiagnosticCategory::TypeError,
            path: String::new(),
            message: format!("type error: {error}"),
            summary_message: None,
        });
    }

    ValidationResult {
        diagnostics,
        config_path: None,
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
#[path = "validate/tests.rs"]
mod tests;
