/// Known provider definitions and auth type enumeration.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthType {
    ApiKey,
    Oauth,
    Local,
}

impl AuthType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ApiKey => "api-key",
            Self::Oauth => "oauth",
            Self::Local => "local",
        }
    }
}

impl std::fmt::Display for AuthType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str((*self).as_str())
    }
}

/// Known provider definitions used to populate the "available providers" list.
pub struct KnownProvider {
    pub name: &'static str,
    pub display_name: &'static str,
    pub auth_type: AuthType,
    pub env_key: Option<&'static str>,
    /// Default base URL for this provider (for OpenAI-compatible providers).
    pub default_base_url: Option<&'static str>,
    /// Whether this provider requires a model to be specified.
    pub requires_model: bool,
    /// Whether the API key is optional (e.g. local backends that run without
    /// auth).
    pub key_optional: bool,
    /// Whether this provider only runs locally (binds to localhost) and should
    /// be hidden from cloud deployments. Separate from `key_optional` because a
    /// remote provider could legitimately support unauthenticated access without
    /// binding to localhost.
    pub local_only: bool,
}

impl KnownProvider {
    /// Returns true if this provider is local-only — runs on the user's
    /// machine and isn't reachable from cloud deployments. Used by cloud-mode
    /// filters to hide providers that bind to localhost.
    #[must_use]
    pub fn is_local_only(&self) -> bool {
        self.auth_type == AuthType::Local || self.local_only
    }
}

/// Build the known providers list at runtime.
pub fn known_providers() -> Vec<KnownProvider> {
    vec![
        // Membership/OAuth providers first — no API key needed, just sign in.
        KnownProvider {
            name: "openai-codex",
            display_name: "OpenAI Codex",
            auth_type: AuthType::Oauth,
            env_key: None,
            default_base_url: None,
            requires_model: false,
            key_optional: false,
            local_only: false,
        },
        KnownProvider {
            name: "github-copilot",
            display_name: "GitHub Copilot",
            auth_type: AuthType::Oauth,
            env_key: None,
            default_base_url: None,
            requires_model: false,
            key_optional: false,
            local_only: false,
        },
        KnownProvider {
            name: "anthropic",
            display_name: "Anthropic",
            auth_type: AuthType::ApiKey,
            env_key: Some("ANTHROPIC_API_KEY"),
            default_base_url: Some("https://api.anthropic.com"),
            requires_model: false,
            key_optional: false,
            local_only: false,
        },
        KnownProvider {
            name: "openai",
            display_name: "OpenAI",
            auth_type: AuthType::ApiKey,
            env_key: Some("OPENAI_API_KEY"),
            default_base_url: Some("https://api.openai.com/v1"),
            requires_model: false,
            key_optional: false,
            local_only: false,
        },
        KnownProvider {
            name: "gemini",
            display_name: "Google Gemini",
            auth_type: AuthType::ApiKey,
            env_key: Some("GEMINI_API_KEY"),
            default_base_url: Some("https://generativelanguage.googleapis.com/v1beta/openai"),
            requires_model: false,
            key_optional: false,
            local_only: false,
        },
        KnownProvider {
            name: "xai",
            display_name: "xAI (Grok)",
            auth_type: AuthType::ApiKey,
            env_key: Some("XAI_API_KEY"),
            default_base_url: Some("https://api.x.ai/v1"),
            requires_model: false,
            key_optional: false,
            local_only: false,
        },
        KnownProvider {
            name: "openrouter",
            display_name: "OpenRouter",
            auth_type: AuthType::ApiKey,
            env_key: Some("OPENROUTER_API_KEY"),
            default_base_url: Some("https://openrouter.ai/api/v1"),
            requires_model: false,
            key_optional: false,
            local_only: false,
        },
        KnownProvider {
            name: "moonshot",
            display_name: "Moonshot",
            auth_type: AuthType::ApiKey,
            env_key: Some("MOONSHOT_API_KEY"),
            default_base_url: Some("https://api.moonshot.cn/v1"),
            requires_model: false,
            key_optional: false,
            local_only: false,
        },
        KnownProvider {
            name: "zai",
            display_name: "Z.AI",
            auth_type: AuthType::ApiKey,
            env_key: Some("Z_API_KEY"),
            default_base_url: Some("https://api.z.ai/api/paas/v4"),
            requires_model: false,
            key_optional: false,
            local_only: false,
        },
        KnownProvider {
            name: "zai-code",
            display_name: "Z.AI (Coding Plan)",
            auth_type: AuthType::ApiKey,
            env_key: Some("Z_CODE_API_KEY"),
            default_base_url: Some("https://api.z.ai/api/coding/paas/v4"),
            requires_model: false,
            key_optional: false,
            local_only: false,
        },
        KnownProvider {
            name: "kimi-code",
            display_name: "Kimi Code",
            auth_type: AuthType::ApiKey,
            env_key: Some("KIMI_API_KEY"),
            default_base_url: Some("https://api.kimi.com/coding/v1"),
            requires_model: false,
            key_optional: false,
            local_only: false,
        },
    ]
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_providers_have_valid_auth_types() {
        for p in known_providers() {
            assert!(
                p.auth_type == AuthType::ApiKey
                    || p.auth_type == AuthType::Oauth
                    || p.auth_type == AuthType::Local,
                "invalid auth type for {}: {}",
                p.name,
                p.auth_type
            );
        }
    }

    #[test]
    fn api_key_providers_have_env_key() {
        for p in known_providers() {
            if p.auth_type == AuthType::ApiKey {
                assert!(
                    p.env_key.is_some(),
                    "api-key provider {} missing env_key",
                    p.name
                );
            }
        }
    }

    #[test]
    fn oauth_providers_have_no_env_key() {
        for p in known_providers() {
            if p.auth_type == AuthType::Oauth {
                assert!(
                    p.env_key.is_none(),
                    "oauth provider {} should not have env_key",
                    p.name
                );
            }
        }
    }

    #[test]
    fn local_providers_have_no_env_key() {
        for p in known_providers() {
            if p.auth_type == AuthType::Local {
                assert!(
                    p.env_key.is_none(),
                    "local provider {} should not have env_key",
                    p.name
                );
            }
        }
    }

    #[test]
    fn known_provider_names_unique() {
        let providers = known_providers();
        let mut names: Vec<&str> = providers.iter().map(|p| p.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), providers.len());
    }

    #[test]
    fn known_providers_include_new_providers() {
        let providers = known_providers();
        let names: Vec<&str> = providers.iter().map(|p| p.name).collect();
        // OpenAI-compatible providers
        assert!(names.contains(&"openrouter"), "missing openrouter");
        assert!(names.contains(&"moonshot"), "missing moonshot");
        assert!(names.contains(&"zai"), "missing zai");
        assert!(names.contains(&"zai-code"), "missing zai-code");
        assert!(names.contains(&"kimi-code"), "missing kimi-code");
        // OAuth providers
        assert!(names.contains(&"github-copilot"), "missing github-copilot");
    }

    #[test]
    fn github_copilot_is_oauth_provider() {
        let providers = known_providers();
        let copilot = providers
            .iter()
            .find(|p| p.name == "github-copilot")
            .expect("github-copilot not in known_providers");
        assert_eq!(copilot.auth_type, AuthType::Oauth);
        assert!(copilot.env_key.is_none());
    }

    #[test]
    fn new_api_key_providers_have_correct_env_keys() {
        let expected = [
            ("openrouter", "OPENROUTER_API_KEY"),
            ("moonshot", "MOONSHOT_API_KEY"),
            ("zai", "Z_API_KEY"),
            ("zai-code", "Z_CODE_API_KEY"),
            ("kimi-code", "KIMI_API_KEY"),
        ];
        let providers = known_providers();
        for (name, env_key) in expected {
            let provider = providers
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("missing provider: {name}"));
            assert_eq!(provider.env_key, Some(env_key), "wrong env_key for {name}");
            assert_eq!(provider.auth_type, AuthType::ApiKey);
        }
    }
}
