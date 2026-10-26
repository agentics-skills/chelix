//! Gateway server URL, proxy, and port checks.

use {
    chelix_config::{ChelixConfig, Diagnostic, DiagnosticCategory, Severity},
    secrecy::ExposeSecret,
};

/// Report server URL, upstream proxy, and ephemeral-port diagnostics.
#[must_use]
pub fn validate_config(config: &ChelixConfig) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    if let Some(url) = config.server.external_url.as_deref() {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            let scheme = url.split("://").next().unwrap_or("<unknown>");
            diagnostics.push(Diagnostic {
                severity: Severity::Error,
                category: DiagnosticCategory::InvalidValue,
                path: "server.external_url".into(),
                message: format!(
                    "server.external_url must use http:// or https:// scheme (got \"{scheme}://\")"
                ),
                summary_message: None,
            });
        }
        if url.ends_with('/') {
            diagnostics.push(Diagnostic {
                severity: Severity::Warning,
                category: DiagnosticCategory::InvalidValue,
                path: "server.external_url".into(),
                message: "server.external_url has a trailing slash; WebAuthn origins must not end with '/' (it will be stripped at runtime)".into(),
                summary_message: None,
            });
        }
    }
    if let Some(proxy) = config.upstream_proxy.as_ref() {
        let url = proxy.expose_secret();
        let valid = url.starts_with("http://")
            || url.starts_with("https://")
            || url.starts_with("socks5://")
            || url.starts_with("socks5h://");
        if !valid {
            // Report the syntactic URI scheme.
            let scheme = url
                .split_once("://")
                .map(|(scheme, _)| scheme)
                .filter(|scheme| {
                    let mut bytes = scheme.bytes();
                    bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic())
                        && bytes.all(|byte| {
                            byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.')
                        })
                });
            let scheme_detail = match scheme {
                Some(scheme) => format!("\"{scheme}://\""),
                None => "no scheme".into(),
            };
            diagnostics.push(Diagnostic {
                severity: Severity::Error,
                category: DiagnosticCategory::InvalidValue,
                path: "upstream_proxy".into(),
                message: format!(
                    "upstream_proxy must use http://, https://, socks5://, or socks5h:// scheme (got {scheme_detail})"
                ),
                summary_message: None,
            });
        }
    }
    if config.server.port == 0 {
        diagnostics.push(Diagnostic {
            severity: Severity::Info,
            category: DiagnosticCategory::Security,
            path: "server.port".into(),
            message: "port is 0; a random port will be assigned at startup".into(),
            summary_message: None,
        });
    }
    diagnostics
}

/// Editor warnings owned by tools, auth, and TLS, in that order.
#[must_use]
pub fn configuration_editor_warnings(config: &ChelixConfig) -> Vec<String> {
    [
        chelix_tools::config_validation::configuration_editor_warning(config),
        chelix_auth::config_validation::configuration_editor_warning(config),
        chelix_tls::config_validation::configuration_editor_warning(config),
    ]
    .into_iter()
    .flatten()
    .map(str::to_string)
    .collect()
}

#[cfg(test)]
mod tests {
    use secrecy::Secret;

    use super::*;

    fn check(config: &ChelixConfig) -> Vec<Diagnostic> {
        validate_config(config)
    }

    fn fixed_port(config: &mut ChelixConfig) {
        config.server.port = 8080;
    }

    #[test]
    fn external_url_bad_scheme_is_error() {
        let mut config = ChelixConfig::default();
        fixed_port(&mut config);
        config.server.external_url = Some("ftp://chelix.example.com".into());
        assert!(check(&config).iter().any(|diagnostic| {
            diagnostic.severity == Severity::Error
                && diagnostic.path == "server.external_url"
                && diagnostic.message.contains("http://")
        }));
    }

    #[test]
    fn external_url_trailing_slash_is_warning() {
        let mut config = ChelixConfig::default();
        fixed_port(&mut config);
        config.server.external_url = Some("https://chelix.example.com/".into());
        assert!(check(&config).iter().any(|diagnostic| {
            diagnostic.severity == Severity::Warning
                && diagnostic.path == "server.external_url"
                && diagnostic.message.contains("trailing slash")
        }));
    }

    #[test]
    fn external_url_valid_schemes_have_no_diagnostics() {
        for url in ["https://chelix.example.com", "http://chelix.local:8080"] {
            let mut config = ChelixConfig::default();
            fixed_port(&mut config);
            config.server.external_url = Some(url.into());
            assert!(
                check(&config)
                    .iter()
                    .all(|diagnostic| diagnostic.path != "server.external_url"),
                "{url}"
            );
        }
    }

    #[test]
    fn upstream_proxy_invalid_scheme_rejected() {
        let mut config = ChelixConfig::default();
        fixed_port(&mut config);
        config.upstream_proxy = Some(Secret::new("ftp://proxy.example.com".into()));
        assert!(check(&config).iter().any(|diagnostic| {
            diagnostic.severity == Severity::Error && diagnostic.path == "upstream_proxy"
        }));
    }

    #[test]
    fn upstream_proxy_valid_schemes_accepted() {
        for scheme in ["http://", "https://", "socks5://", "socks5h://"] {
            let mut config = ChelixConfig::default();
            fixed_port(&mut config);
            config.upstream_proxy = Some(Secret::new(format!("{scheme}proxy.example.com:1080")));
            assert!(
                check(&config)
                    .iter()
                    .all(|diagnostic| diagnostic.path != "upstream_proxy"),
                "{scheme}"
            );
        }
    }

    #[test]
    fn upstream_proxy_diagnostic_keeps_credentials_out_of_the_message() {
        for (url, detail) in [
            ("ftp://user:super-secret@proxy.example.com", "\"ftp://\""),
            ("http//user:super-secret@proxy.example.com", "no scheme"),
            ("user:super-secret@proxy.example.com://x", "no scheme"),
        ] {
            let mut config = ChelixConfig::default();
            fixed_port(&mut config);
            config.upstream_proxy = Some(Secret::new(url.into()));
            let expected = format!(
                "upstream_proxy must use http://, https://, socks5://, or socks5h:// scheme (got {detail})"
            );
            assert!(check(&config).iter().any(|diagnostic| {
                diagnostic.path == "upstream_proxy" && diagnostic.message == expected
            }));
        }
    }

    #[test]
    fn port_zero_info() {
        let diagnostics = check(&ChelixConfig::default());
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.severity == Severity::Info
                && diagnostic.path == "server.port"
                && diagnostic.message.contains("random port")
        }));
    }
}
