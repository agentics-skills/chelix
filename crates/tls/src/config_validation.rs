//! TLS exposure, certificate pairing, and certificate file checks.

use std::path::Path;

use chelix_config::{ChelixConfig, Diagnostic, DiagnosticCategory, Severity};

/// Report TLS bind, certificate, public-IP, and certificate-file diagnostics.
///
/// Certificate paths are checked exactly as configured.
#[must_use]
pub fn validate_config(config: &ChelixConfig) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    check_bind_and_material(config, &mut diagnostics);
    check_certificate_files(config, &mut diagnostics);
    diagnostics
}

/// Editor warning when the gateway serves unencrypted HTTP.
#[must_use]
pub fn configuration_editor_warning(config: &ChelixConfig) -> Option<&'static str> {
    if config.tls.enabled {
        None
    } else {
        Some("TLS is disabled. Connections will use unencrypted HTTP.")
    }
}

fn check_bind_and_material(config: &ChelixConfig, diagnostics: &mut Vec<Diagnostic>) {
    if !config.tls.enabled && !config.server.binds_localhost() {
        diagnostics.push(Diagnostic {
            severity: Severity::Warning,
            summary_message: None,
            category: DiagnosticCategory::Security,
            path: "tls".into(),
            message: format!("TLS is disabled while binding to {}", config.server.bind),
        });
    }

    let has_cert = config.tls.cert_path.is_some();
    let has_key = config.tls.key_path.is_some();
    if has_cert && !has_key {
        diagnostics.push(Diagnostic {
            severity: Severity::Error,
            summary_message: None,
            category: DiagnosticCategory::Security,
            path: "tls".into(),
            message: "tls.cert_path is set but tls.key_path is missing".into(),
        });
    }
    if has_key && !has_cert {
        diagnostics.push(Diagnostic {
            severity: Severity::Error,
            summary_message: None,
            category: DiagnosticCategory::Security,
            path: "tls".into(),
            message: "tls.key_path is set but tls.cert_path is missing".into(),
        });
    }
    if let Some(public_ip) = config.tls.public_ip.as_deref() {
        match public_ip.parse::<std::net::IpAddr>() {
            Ok(ip) if ip.is_loopback() || ip.is_unspecified() => {
                diagnostics.push(Diagnostic {
                    severity: Severity::Warning,
                    summary_message: None,
                    category: DiagnosticCategory::Security,
                    path: "tls.public_ip".into(),
                    message: "tls.public_ip is a loopback or unspecified address and will not be useful as a public IP SAN".into(),
                });
            },
            Ok(_) => {},
            Err(_) => {
                diagnostics.push(Diagnostic {
                    severity: Severity::Error,
                    summary_message: None,
                    category: DiagnosticCategory::Security,
                    path: "tls.public_ip".into(),
                    message: "tls.public_ip must be an IPv4 or IPv6 address".into(),
                });
            },
        }
        if has_cert && has_key {
            diagnostics.push(Diagnostic {
                severity: Severity::Warning,
                summary_message: None,
                category: DiagnosticCategory::Security,
                path: "tls.public_ip".into(),
                message: "tls.public_ip has no effect when tls.cert_path and tls.key_path are set"
                    .into(),
            });
        }
    }
}

fn check_certificate_files(config: &ChelixConfig, diagnostics: &mut Vec<Diagnostic>) {
    let file_refs: &[(&str, &Option<String>)] = &[
        ("tls.cert_path", &config.tls.cert_path),
        ("tls.key_path", &config.tls.key_path),
        ("tls.ca_cert_path", &config.tls.ca_cert_path),
    ];
    for (path_name, value) in file_refs {
        if let Some(file_path) = value {
            let candidate = Path::new(file_path);
            if !candidate.exists() {
                diagnostics.push(Diagnostic {
                    severity: Severity::Warning,
                    summary_message: None,
                    category: DiagnosticCategory::FileRef,
                    path: (*path_name).into(),
                    message: format!("file not found: {file_path}"),
                });
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn check(config: &ChelixConfig) -> Vec<Diagnostic> {
        validate_config(config)
    }

    #[test]
    fn editor_warning_tracks_tls_enabled() {
        assert_eq!(configuration_editor_warning(&ChelixConfig::default()), None);
        let mut config = ChelixConfig::default();
        config.tls.enabled = false;
        assert_eq!(
            configuration_editor_warning(&config),
            Some("TLS is disabled. Connections will use unencrypted HTTP.")
        );
    }

    #[test]
    fn tls_disabled_non_localhost_warned() {
        let mut config = ChelixConfig::default();
        config.server.bind = "0.0.0.0".into();
        config.tls.enabled = false;
        assert!(check(&config).iter().any(|diagnostic| {
            diagnostic.category == DiagnosticCategory::Security
                && diagnostic.path == "tls"
                && diagnostic.severity == Severity::Warning
        }));
    }

    #[test]
    fn tls_disabled_localhost_not_warned() {
        let mut config = ChelixConfig::default();
        config.tls.enabled = false;
        assert!(check(&config).is_empty());
    }

    #[test]
    fn tls_cert_without_key_is_error() {
        let mut config = ChelixConfig::default();
        config.tls.cert_path = Some("/path/to/cert.pem".into());
        assert!(check(&config).iter().any(|diagnostic| {
            diagnostic.severity == Severity::Error
                && diagnostic.path == "tls"
                && diagnostic.message.contains("key_path")
        }));
    }

    #[test]
    fn tls_key_without_cert_is_error() {
        let mut config = ChelixConfig::default();
        config.tls.key_path = Some("/path/to/key.pem".into());
        assert!(check(&config).iter().any(|diagnostic| {
            diagnostic.severity == Severity::Error
                && diagnostic.path == "tls"
                && diagnostic.message.contains("cert_path")
        }));
    }

    #[test]
    fn tls_public_ip_accepts_ip_address() {
        let mut config = ChelixConfig::default();
        config.tls.public_ip = Some("203.0.113.10".into());
        assert!(
            check(&config)
                .iter()
                .all(|diagnostic| diagnostic.path != "tls.public_ip")
        );
    }

    #[test]
    fn tls_public_ip_rejects_dns_name() {
        let mut config = ChelixConfig::default();
        config.tls.public_ip = Some("chat.example.com".into());
        assert!(check(&config).iter().any(|diagnostic| {
            diagnostic.severity == Severity::Error && diagnostic.path == "tls.public_ip"
        }));
    }

    #[test]
    fn tls_public_ip_warns_for_loopback_or_unspecified_address() {
        for public_ip in ["127.0.0.1", "::1", "0.0.0.0", "::"] {
            let mut config = ChelixConfig::default();
            config.tls.public_ip = Some(public_ip.into());
            assert!(
                check(&config).iter().any(|diagnostic| {
                    diagnostic.severity == Severity::Warning
                        && diagnostic.path == "tls.public_ip"
                        && diagnostic.message.contains("loopback or unspecified")
                }),
                "expected warning for {public_ip}"
            );
        }
    }

    #[test]
    fn tls_public_ip_warns_with_custom_cert_paths() {
        let mut config = ChelixConfig::default();
        config.tls.cert_path = Some("/path/to/cert.pem".into());
        config.tls.key_path = Some("/path/to/key.pem".into());
        config.tls.public_ip = Some("203.0.113.10".into());
        assert!(check(&config).iter().any(|diagnostic| {
            diagnostic.severity == Severity::Warning
                && diagnostic.path == "tls.public_ip"
                && diagnostic.message.contains("has no effect")
        }));
    }

    #[test]
    fn missing_certificate_files_are_warnings() {
        let mut config = ChelixConfig::default();
        config.tls.cert_path = Some("/no/such/chelix-cert.pem".into());
        config.tls.key_path = Some("/no/such/chelix-key.pem".into());
        config.tls.ca_cert_path = Some("/no/such/chelix-ca.pem".into());
        let diagnostics = check(&config);
        for path in ["tls.cert_path", "tls.key_path", "tls.ca_cert_path"] {
            assert!(diagnostics.iter().any(|diagnostic| {
                diagnostic.severity == Severity::Warning
                    && diagnostic.category == DiagnosticCategory::FileRef
                    && diagnostic.path == path
                    && diagnostic.message.contains("file not found")
            }));
        }
    }

    #[test]
    fn existing_certificate_files_are_accepted() {
        let file =
            std::env::temp_dir().join(format!("chelix-tls-validation-{}.pem", std::process::id()));
        std::fs::write(&file, b"pem").unwrap();
        let path = file.display().to_string();
        let mut config = ChelixConfig::default();
        config.tls.cert_path = Some(path.clone());
        config.tls.key_path = Some(path.clone());
        config.tls.ca_cert_path = Some(path);
        let diagnostics = check(&config);
        let _ = std::fs::remove_file(&file);
        assert!(
            diagnostics
                .iter()
                .all(|diagnostic| diagnostic.category != DiagnosticCategory::FileRef)
        );
    }
}
