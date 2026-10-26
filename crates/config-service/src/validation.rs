use std::{path::Path, sync::Arc};

use chelix_config::{ChelixConfig, Diagnostic, DiagnosticCategory, Severity, ValidationResult};

use crate::{Error, Result, snapshot::Snapshot, storage::Storage};

#[async_trait::async_trait]
pub trait ConfigValidator: Send + Sync {
    async fn validate(&self, candidate: Arc<ChelixConfig>) -> Vec<Diagnostic>;
}

pub struct SyncValidator(pub fn(&ChelixConfig) -> Vec<Diagnostic>);

#[async_trait::async_trait]
impl ConfigValidator for SyncValidator {
    async fn validate(&self, candidate: Arc<ChelixConfig>) -> Vec<Diagnostic> {
        (self.0)(&candidate)
    }
}

pub struct BlockingValidator(pub fn(&ChelixConfig) -> Vec<Diagnostic>);

#[async_trait::async_trait]
impl ConfigValidator for BlockingValidator {
    async fn validate(&self, candidate: Arc<ChelixConfig>) -> Vec<Diagnostic> {
        let check = self.0;
        match tokio::task::spawn_blocking(move || check(&candidate)).await {
            Ok(diagnostics) => diagnostics,
            Err(error) => vec![Diagnostic {
                severity: Severity::Error,
                category: DiagnosticCategory::ValidationWorker,
                path: String::new(),
                message: format!("configuration validation worker failed: {error}"),
                summary_message: None,
            }],
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum ValidationScope {
    Load,
    Full,
}

#[derive(Clone, Default)]
pub struct ConfigValidation {
    checks: Arc<Vec<Arc<dyn ConfigValidator>>>,
    load_checks: Arc<Vec<Arc<dyn ConfigValidator>>>,
}

impl ConfigValidation {
    #[must_use]
    pub fn new(
        checks: Vec<Arc<dyn ConfigValidator>>,
        load_checks: Vec<Arc<dyn ConfigValidator>>,
    ) -> Self {
        Self {
            checks: Arc::new(checks),
            load_checks: Arc::new(load_checks),
        }
    }

    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
    pub async fn diagnose(&self, raw: &str, path: &Path) -> ValidationResult {
        self.inspect(raw, path, ValidationScope::Full, None).await.1
    }

    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all))]
    pub async fn diagnose_file(&self) -> ValidationResult {
        let storage = match Storage::discover() {
            Ok(storage) => storage,
            Err(error) => return failed_diagnostic(None, error.to_string()),
        };
        match storage.read().await {
            Ok(raw) => self.diagnose(&raw, &storage.path).await,
            Err(Error::Read { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
                ValidationResult {
                    diagnostics: vec![Diagnostic {
                        severity: Severity::Info,
                        category: DiagnosticCategory::FileRef,
                        path: String::new(),
                        message: "no config file found; startup initialization will create one"
                            .into(),
                        summary_message: None,
                    }],
                    config_path: None,
                }
            },
            Err(error) => failed_diagnostic(Some(storage.path), error.to_string()),
        }
    }

    pub(crate) async fn admit(
        &self,
        raw: String,
        path: &Path,
        scope: ValidationScope,
        extra: Option<&dyn ConfigValidator>,
    ) -> Result<Snapshot> {
        let (candidate, diagnostics) = self.inspect(&raw, path, scope, extra).await;
        #[cfg(feature = "tracing")]
        for diagnostic in diagnostics
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == Severity::Error)
        {
            tracing::error!(
                path = %diagnostic.path,
                category = %diagnostic.category,
                message = diagnostic_summary(diagnostic),
            );
        }
        if diagnostics.has_errors() {
            return Err(Error::Validation(format_diagnostics(
                &diagnostics.diagnostics,
            )));
        }
        let config =
            candidate.ok_or_else(|| Error::Worker("validated candidate is unavailable".into()))?;
        Ok(Snapshot { raw, config })
    }

    async fn inspect(
        &self,
        raw: &str,
        path: &Path,
        scope: ValidationScope,
        extra: Option<&dyn ConfigValidator>,
    ) -> (Option<Arc<ChelixConfig>>, ValidationResult) {
        let mut result = chelix_config::validate::validate_toml_str(raw);
        result.config_path = Some(path.to_path_buf());
        if result.has_errors() {
            let summary = crate::error::admission_log_reason(raw, path);
            for diagnostic in result.diagnostics.iter_mut().filter(|diagnostic| {
                diagnostic.severity == Severity::Error && diagnostic.path.is_empty()
            }) {
                diagnostic.summary_message = summary.clone();
            }
            return (None, result);
        }
        let candidate = match chelix_config::parse_config_source(raw, path) {
            Ok(candidate) => Arc::new(candidate),
            Err(error) => {
                result.diagnostics.push(Diagnostic {
                    severity: Severity::Error,
                    category: DiagnosticCategory::TypeError,
                    path: String::new(),
                    message: error.to_string(),
                    summary_message: crate::error::admission_log_reason(raw, path),
                });
                return (None, result);
            },
        };
        let checks = match scope {
            ValidationScope::Load => &self.load_checks,
            ValidationScope::Full => &self.checks,
        };
        for check in checks.iter() {
            result
                .diagnostics
                .extend(check.validate(Arc::clone(&candidate)).await);
        }
        if let Some(check) = extra {
            result
                .diagnostics
                .extend(check.validate(Arc::clone(&candidate)).await);
        }
        (Some(candidate), result)
    }
}

fn failed_diagnostic(path: Option<std::path::PathBuf>, message: String) -> ValidationResult {
    ValidationResult {
        diagnostics: vec![Diagnostic {
            severity: Severity::Error,
            category: DiagnosticCategory::FileRef,
            path: String::new(),
            message,
            summary_message: None,
        }],
        config_path: path,
    }
}

fn diagnostic_summary(diagnostic: &Diagnostic) -> &str {
    diagnostic
        .summary_message
        .as_deref()
        .unwrap_or(&diagnostic.message)
}

fn format_diagnostics(diagnostics: &[Diagnostic]) -> String {
    diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .map(|diagnostic| {
            let message = diagnostic_summary(diagnostic);
            if diagnostic.path.is_empty() {
                message.to_owned()
            } else {
                format!("{}: {message}", diagnostic.path)
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}
