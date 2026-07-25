//! OpenAI-compatible provider transport definitions.

use crate::openai::{CacheControlPolicy, OpenAiProviderCapabilities};

/// OpenAI-compatible provider definition for table-driven registration.
pub(crate) struct OpenAiCompatDef {
    pub(crate) config_name: &'static str,
    pub(crate) env_key: &'static str,
    pub(crate) env_base_url_key: &'static str,
    pub(crate) default_base_url: &'static str,
    /// When `false`, a dummy API key (the provider name) is used if none is
    /// configured. Intended for local servers that don't authenticate.
    pub(crate) requires_api_key: bool,
    /// Local-only providers are skipped unless the user has an explicit
    /// `[providers.<name>]` entry, a `_BASE_URL` env var, or configured models.
    /// This avoids probing localhost when nothing is running. Also ensures
    /// model discovery is always attempted (never short-circuited by the
    /// empty-catalog heuristic).
    pub(crate) local_only: bool,
    /// Explicit provider behavior policies. Never inferred from provider name or URL.
    pub(crate) capabilities: OpenAiProviderCapabilities,
}

impl OpenAiCompatDef {
    const DEFAULT: Self = Self {
        config_name: "",
        env_key: "",
        env_base_url_key: "",
        default_base_url: "",
        requires_api_key: true,
        local_only: false,
        capabilities: OpenAiProviderCapabilities::DEFAULT,
    };
}

pub(crate) const OPENAI_COMPAT_PROVIDERS: &[OpenAiCompatDef] = &[
    OpenAiCompatDef {
        config_name: "openrouter",
        env_key: "OPENROUTER_API_KEY",
        env_base_url_key: "OPENROUTER_BASE_URL",
        default_base_url: "https://openrouter.ai/api/v1",
        capabilities: OpenAiProviderCapabilities {
            cache_control_policy: CacheControlPolicy::OpenRouterAnthropic,
            ..OpenAiProviderCapabilities::DEFAULT
        },
        ..OpenAiCompatDef::DEFAULT
    },
    OpenAiCompatDef {
        config_name: "moonshot",
        env_key: "MOONSHOT_API_KEY",
        env_base_url_key: "MOONSHOT_BASE_URL",
        default_base_url: "https://api.moonshot.ai/v1",
        capabilities: OpenAiProviderCapabilities {
            default_reasoning_content_on_tool_messages: true,
            ..OpenAiProviderCapabilities::DEFAULT
        },
        ..OpenAiCompatDef::DEFAULT
    },
    OpenAiCompatDef {
        config_name: "zai",
        env_key: "Z_API_KEY",
        env_base_url_key: "Z_BASE_URL",
        default_base_url: "https://api.z.ai/api/paas/v4",
        ..OpenAiCompatDef::DEFAULT
    },
    OpenAiCompatDef {
        config_name: "zai-code",
        env_key: "Z_CODE_API_KEY",
        env_base_url_key: "Z_CODE_BASE_URL",
        default_base_url: "https://api.z.ai/api/coding/paas/v4",
        ..OpenAiCompatDef::DEFAULT
    },
    OpenAiCompatDef {
        config_name: "deepinfra",
        env_key: "DEEPINFRA_API_KEY",
        env_base_url_key: "DEEPINFRA_BASE_URL",
        default_base_url: "https://api.deepinfra.com/v1/openai",
        ..OpenAiCompatDef::DEFAULT
    },
    OpenAiCompatDef {
        config_name: "alibaba-coding",
        env_key: "ALIBABA_CODING_API_KEY",
        env_base_url_key: "ALIBABA_CODING_BASE_URL",
        default_base_url: "https://coding-intl.dashscope.aliyuncs.com/v1",
        capabilities: OpenAiProviderCapabilities {
            requires_single_leading_system_message: true,
            ..OpenAiProviderCapabilities::DEFAULT
        },
        ..OpenAiCompatDef::DEFAULT
    },
    OpenAiCompatDef {
        config_name: "gemini",
        env_key: "GEMINI_API_KEY",
        env_base_url_key: "GEMINI_BASE_URL",
        default_base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
        capabilities: OpenAiProviderCapabilities {
            requires_gemini_tool_call_extra_content: true,
            ..OpenAiProviderCapabilities::DEFAULT
        },
        ..OpenAiCompatDef::DEFAULT
    },
];

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn openai_compat_providers_have_unique_names() {
        let mut names: Vec<&str> = OPENAI_COMPAT_PROVIDERS
            .iter()
            .map(|d| d.config_name)
            .collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), OPENAI_COMPAT_PROVIDERS.len());
    }

    #[test]
    fn openai_compat_providers_have_valid_urls() {
        for def in OPENAI_COMPAT_PROVIDERS {
            assert!(
                def.default_base_url.starts_with("http://")
                    || def.default_base_url.starts_with("https://"),
                "{}: invalid base URL: {}",
                def.config_name,
                def.default_base_url
            );
        }
    }

    #[test]
    fn openai_compat_providers_env_keys_not_empty() {
        for def in OPENAI_COMPAT_PROVIDERS {
            assert!(
                !def.env_key.is_empty(),
                "{}: env_key is empty",
                def.config_name
            );
            assert!(
                !def.env_base_url_key.is_empty(),
                "{}: env_base_url_key is empty",
                def.config_name
            );
        }
    }

    #[test]
    fn alibaba_coding_provider_exists() {
        let alibaba = OPENAI_COMPAT_PROVIDERS
            .iter()
            .find(|d| d.config_name == "alibaba-coding")
            .expect("alibaba-coding entry must exist");
        assert_eq!(alibaba.env_key, "ALIBABA_CODING_API_KEY");
        assert_eq!(
            alibaba.default_base_url,
            "https://coding-intl.dashscope.aliyuncs.com/v1"
        );
        assert!(alibaba.requires_api_key);
        assert!(!alibaba.local_only);
    }

    /// Cross-validate that every provider registered in this crate appears in
    /// the canonical `KNOWN_PROVIDER_NAMES` list in `chelix-config`.
    ///
    /// If this test fails, you added a provider to `chelix-providers` without
    /// updating `crates/config/src/schema/providers.rs::KNOWN_PROVIDER_NAMES`.
    #[test]
    fn all_registered_providers_in_canonical_known_list() {
        use chelix_config::schema::KNOWN_PROVIDER_NAMES;

        // Built-in providers
        let mut provider_names: Vec<&str> = vec!["anthropic", "openai"];

        // OpenAI-compatible table-driven providers
        for def in OPENAI_COMPAT_PROVIDERS {
            provider_names.push(def.config_name);
        }

        // Feature-gated providers (always check names, regardless of feature).
        //
        // NOTE: This list must be maintained manually because `#[cfg(feature)]`
        // attributes make it impossible to discover these names at test time
        // when the feature is disabled.  When adding a new feature-gated
        // provider registration in `registry/registration.rs` (e.g. a new
        // `register_*_providers` method gated behind a cargo feature), add its
        // config name here too.
        provider_names.extend_from_slice(&["github-copilot", "kimi-code", "openai-codex", "xai"]);

        for name in &provider_names {
            assert!(
                KNOWN_PROVIDER_NAMES.contains(name),
                "provider \"{name}\" is registered in chelix-providers but missing from \
                 KNOWN_PROVIDER_NAMES in crates/config/src/schema/providers.rs — add it there"
            );
        }
    }

    /// Ensure `KNOWN_PROVIDER_NAMES` has no duplicates.
    #[test]
    fn canonical_known_list_has_no_duplicates() {
        use chelix_config::schema::KNOWN_PROVIDER_NAMES;

        let mut sorted: Vec<&str> = KNOWN_PROVIDER_NAMES.to_vec();
        sorted.sort();
        for window in sorted.windows(2) {
            assert_ne!(
                window[0], window[1],
                "duplicate entry \"{0}\" in KNOWN_PROVIDER_NAMES",
                window[0]
            );
        }
    }
}
