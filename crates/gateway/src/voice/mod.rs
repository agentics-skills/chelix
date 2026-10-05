//! Voice service implementations for TTS and STT.
//!
//! This module provides concrete implementations of the `TtsService` and
//! `SttService` traits using the chelix-voice crate's providers.

#[cfg(feature = "voice")]
mod stt_service;
#[cfg(feature = "voice")]
mod tts_service;

#[cfg(feature = "voice")]
pub use stt_service::{LiveSttService, SttServiceConfig};
#[cfg(feature = "voice")]
pub use tts_service::LiveTtsService;

// `SttService` trait and `NoopSttService` are defined in `chelix-service-traits`
// and re-exported via `crate::services::*`.
pub use crate::services::{NoopSttService, SttService};

#[cfg(feature = "voice")]
use secrecy::Secret;

// TTS/STT provider IDs are defined once in chelix-config (VoiceTtsProvider,
// VoiceSttProvider) and re-exported through chelix-voice as TtsProviderId /
// SttProviderId. Same type everywhere — no conversion needed.

/// Load voice settings from `chelix.toml`.
#[cfg(feature = "voice")]
pub(crate) fn load_voice_config() -> anyhow::Result<chelix_config::ChelixConfig> {
    Ok(chelix_config::discover_and_load()?)
}

#[cfg(feature = "voice")]
pub(crate) fn whisper_api_key(cfg: &chelix_config::ChelixConfig) -> Option<Secret<String>> {
    nonempty_secret(cfg.voice.stt.whisper.api_key.as_ref())
}

#[cfg(feature = "voice")]
pub(crate) fn whisper_key_configured(cfg: &chelix_config::ChelixConfig) -> bool {
    whisper_api_key(cfg).is_some()
}

#[cfg(feature = "voice")]
fn nonempty_secret(key: Option<&Secret<String>>) -> Option<Secret<String>> {
    use secrecy::ExposeSecret;
    key.filter(|value| !value.expose_secret().trim().is_empty())
        .cloned()
}

#[cfg(feature = "voice")]
pub(crate) fn resolve_openai_key(voice_key: Option<&Secret<String>>) -> Option<Secret<String>> {
    nonempty_secret(voice_key)
}

#[cfg(feature = "voice")]
pub(crate) fn resolve_openai_tts_base_url(cfg: &chelix_config::ChelixConfig) -> Option<String> {
    cfg.voice.tts.openai.base_url.clone()
}

#[cfg(feature = "voice")]
pub(crate) fn resolve_openai_whisper_base_url(cfg: &chelix_config::ChelixConfig) -> Option<String> {
    cfg.voice.stt.whisper.base_url.clone()
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(all(test, feature = "voice"))]
mod tests {
    use {super::*, secrecy::ExposeSecret};

    #[test]
    fn test_resolve_openai_key_prefers_voice_key_over_llm_provider_key() {
        let resolved = resolve_openai_key(Some(&Secret::new("voice-openai-key".to_string())))
            .map(|value| value.expose_secret().to_string());
        assert_eq!(resolved.as_deref(), Some("voice-openai-key"));
    }

    #[test]
    fn test_resolve_openai_key_uses_llm_provider_key_when_voice_key_missing() {
        let resolved = resolve_openai_key(None).map(|value| value.expose_secret().to_string());
        assert_eq!(resolved.as_deref(), None);
    }

    #[test]
    fn test_resolve_openai_tts_base_url_prefers_voice_specific_value() {
        let mut cfg = chelix_config::ChelixConfig::default();
        cfg.voice.tts.openai.base_url = Some("http://127.0.0.1:8003".to_string());
        cfg.providers.providers.insert(
            "openai".to_string(),
            chelix_config::schema::ProviderEntry {
                base_url: Some("http://127.0.0.1:8001".to_string()),
                ..chelix_config::schema::ProviderEntry::default()
            },
        );

        assert_eq!(
            resolve_openai_tts_base_url(&cfg).as_deref(),
            Some("http://127.0.0.1:8003")
        );
    }

    #[test]
    fn test_resolve_openai_tts_base_url_falls_back_to_provider_value() {
        let mut cfg = chelix_config::ChelixConfig::default();
        cfg.providers.providers.insert(
            "openai".to_string(),
            chelix_config::schema::ProviderEntry {
                base_url: Some("http://127.0.0.1:8001".to_string()),
                ..chelix_config::schema::ProviderEntry::default()
            },
        );

        assert_eq!(resolve_openai_tts_base_url(&cfg).as_deref(), None);
    }

    #[test]
    fn test_resolve_openai_whisper_base_url_prefers_voice_specific_value() {
        let mut cfg = chelix_config::ChelixConfig::default();
        cfg.voice.stt.whisper.base_url = Some("http://127.0.0.1:8002".to_string());
        cfg.providers.providers.insert(
            "openai".to_string(),
            chelix_config::schema::ProviderEntry {
                base_url: Some("http://127.0.0.1:8001".to_string()),
                ..chelix_config::schema::ProviderEntry::default()
            },
        );

        assert_eq!(
            resolve_openai_whisper_base_url(&cfg).as_deref(),
            Some("http://127.0.0.1:8002")
        );
    }

    #[test]
    fn test_resolve_openai_whisper_base_url_falls_back_to_provider_value() {
        let mut cfg = chelix_config::ChelixConfig::default();
        cfg.providers.providers.insert(
            "openai".to_string(),
            chelix_config::schema::ProviderEntry {
                base_url: Some("http://127.0.0.1:8001".to_string()),
                ..chelix_config::schema::ProviderEntry::default()
            },
        );

        assert_eq!(resolve_openai_whisper_base_url(&cfg).as_deref(), None);
    }
}
