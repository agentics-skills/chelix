//! Voice TTS and STT provider-name checks.

use chelix_config::{ChelixConfig, Diagnostic, DiagnosticCategory, Severity};

const VALID_VOICE_TTS_PROVIDERS: &[&str] = &[
    "elevenlabs",
    "openai",
    "openai-tts",
    "google",
    "google-tts",
    "piper",
    "coqui",
];

const VALID_VOICE_STT_PROVIDERS: &[&str] = &[
    "whisper",
    "deepgram",
    "google",
    "elevenlabs",
    "elevenlabs-stt",
    "voxtral-local",
    "whisper-local",
    "whisper-cli",
    "sherpa-onnx",
];

/// Warn when a listed TTS or STT provider name is not one of the known ids.
#[must_use]
pub fn validate_config(config: &ChelixConfig) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for (idx, provider) in config.voice.tts.providers.iter().enumerate() {
        if !VALID_VOICE_TTS_PROVIDERS.contains(&provider.as_str()) {
            diagnostics.push(Diagnostic {
                severity: Severity::Warning,
                summary_message: None,
                category: DiagnosticCategory::UnknownField,
                path: format!("voice.tts.providers[{idx}]"),
                message: format!(
                    "unknown TTS provider \"{provider}\"; expected one of: {}",
                    VALID_VOICE_TTS_PROVIDERS.join(", ")
                ),
            });
        }
    }
    for (idx, provider) in config.voice.stt.providers.iter().enumerate() {
        if !VALID_VOICE_STT_PROVIDERS.contains(&provider.as_str()) {
            diagnostics.push(Diagnostic {
                severity: Severity::Warning,
                summary_message: None,
                category: DiagnosticCategory::UnknownField,
                path: format!("voice.stt.providers[{idx}]"),
                message: format!(
                    "unknown STT provider \"{provider}\"; expected one of: {}",
                    VALID_VOICE_STT_PROVIDERS.join(", ")
                ),
            });
        }
    }
    diagnostics
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(config: &ChelixConfig) -> Vec<Diagnostic> {
        validate_config(config)
    }

    #[test]
    fn unknown_voice_tts_list_provider_warned() {
        let mut config = ChelixConfig::default();
        config.voice.tts.providers = vec!["openai-tts".into(), "not-a-provider".into()];
        assert!(check(&config).iter().any(|diagnostic| {
            diagnostic.path == "voice.tts.providers[1]" && diagnostic.severity == Severity::Warning
        }));
    }

    #[test]
    fn unknown_voice_stt_list_provider_warned() {
        let mut config = ChelixConfig::default();
        config.voice.stt.providers = vec!["whisper".into(), "not-a-provider".into()];
        assert!(check(&config).iter().any(|diagnostic| {
            diagnostic.path == "voice.stt.providers[1]" && diagnostic.severity == Severity::Warning
        }));
    }

    #[test]
    fn known_voice_provider_list_entries_not_warned() {
        let mut config = ChelixConfig::default();
        config.voice.tts.providers = vec!["openai".into(), "google-tts".into(), "coqui".into()];
        config.voice.stt.providers = vec![
            "elevenlabs".into(),
            "whisper-cli".into(),
            "sherpa-onnx".into(),
        ];
        let warnings: Vec<_> = check(&config)
            .into_iter()
            .filter(|diagnostic| {
                diagnostic.category == DiagnosticCategory::UnknownField
                    && (diagnostic.path.starts_with("voice.tts.providers")
                        || diagnostic.path.starts_with("voice.stt.providers"))
            })
            .collect();
        assert!(warnings.is_empty());
    }
}
