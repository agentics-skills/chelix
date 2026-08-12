/// Known provider definitions used to populate the available providers list.
pub struct KnownProvider {
    pub name: &'static str,
    pub display_name: &'static str,
    pub env_key: &'static str,
    /// Default base URL for this provider (for OpenAI-compatible providers).
    pub default_base_url: Option<&'static str>,
    /// Whether this provider requires a model to be specified.
    pub requires_model: bool,
    /// Whether the API key is optional.
    pub key_optional: bool,
    /// Whether this provider only runs locally and should be hidden from cloud deployments.
    pub local_only: bool,
}

impl KnownProvider {
    /// Returns true if this provider should be hidden from cloud deployments.
    #[must_use]
    pub fn is_local_only(&self) -> bool {
        self.local_only
    }
}

/// Build the known providers list at runtime.
pub fn known_providers() -> Vec<KnownProvider> {
    vec![
        KnownProvider {
            name: "openai",
            display_name: "OpenAI",
            env_key: "OPENAI_API_KEY",
            default_base_url: Some("https://api.openai.com/v1"),
            requires_model: false,
            key_optional: false,
            local_only: false,
        },
        KnownProvider {
            name: "openrouter",
            display_name: "OpenRouter",
            env_key: "OPENROUTER_API_KEY",
            default_base_url: Some("https://openrouter.ai/api/v1"),
            requires_model: false,
            key_optional: false,
            local_only: false,
        },
        KnownProvider {
            name: "zai",
            display_name: "Z.AI",
            env_key: "Z_API_KEY",
            default_base_url: Some("https://api.z.ai/api/paas/v4"),
            requires_model: false,
            key_optional: false,
            local_only: false,
        },
        KnownProvider {
            name: "zai-code",
            display_name: "Z.AI (Coding Plan)",
            env_key: "Z_CODE_API_KEY",
            default_base_url: Some("https://api.z.ai/api/coding/paas/v4"),
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
    fn providers_have_env_keys() {
        for provider in known_providers() {
            assert!(
                !provider.env_key.is_empty(),
                "{} missing env_key",
                provider.name
            );
        }
    }

    #[test]
    fn known_provider_names_unique() {
        let providers = known_providers();
        let mut names: Vec<&str> = providers.iter().map(|provider| provider.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), providers.len());
    }

    #[test]
    fn known_providers_include_openai_compatible_providers() {
        let providers = known_providers();
        let names: Vec<&str> = providers.iter().map(|provider| provider.name).collect();
        assert!(names.contains(&"openrouter"), "missing openrouter");
        assert!(names.contains(&"zai"), "missing zai");
        assert!(names.contains(&"zai-code"), "missing zai-code");
    }

    #[test]
    fn providers_have_correct_env_keys() {
        let expected = [
            ("openai", "OPENAI_API_KEY"),
            ("openrouter", "OPENROUTER_API_KEY"),
            ("zai", "Z_API_KEY"),
            ("zai-code", "Z_CODE_API_KEY"),
        ];
        let providers = known_providers();
        for (name, env_key) in expected {
            let provider = providers
                .iter()
                .find(|provider| provider.name == name)
                .unwrap_or_else(|| panic!("missing provider: {name}"));
            assert_eq!(provider.env_key, env_key, "wrong env_key for {name}");
        }
    }
}
