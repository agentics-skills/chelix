//! Live STT service implementation.

use std::sync::Arc;

use {
    async_trait::async_trait,
    base64::Engine,
    serde_json::{Value, json},
    tracing::{debug, warn},
};

use chelix_voice::{
    AudioFormat, DeepgramStt, ElevenLabsStt, GoogleStt, SherpaOnnxStt, SttProvider, SttProviderId,
    TranscribeRequest, VoxtralLocalStt, WhisperCliStt, WhisperLocalStt, WhisperStt,
};

use crate::services::{ServiceResult, SttService};

use super::{load_voice_config, resolve_openai_whisper_base_url};

/// Live STT service that delegates to voice providers.
/// Reads the config service on each operation so later edits are visible.
pub struct LiveSttService {
    config_service: chelix_config_service::ConfigService,
}

impl std::fmt::Debug for LiveSttService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveSttService").finish()
    }
}

impl LiveSttService {
    pub fn new(config_service: chelix_config_service::ConfigService) -> Self {
        Self { config_service }
    }

    fn config_service(&self) -> &chelix_config_service::ConfigService {
        &self.config_service
    }

    async fn load_config(
        &self,
    ) -> Result<Arc<chelix_config::ChelixConfig>, crate::services::ServiceError> {
        load_voice_config(self.config_service())
            .await
            .map_err(crate::services::ServiceError::message)
    }

    /// Create a provider on demand.
    fn create_provider(
        cfg: &chelix_config::ChelixConfig,
        provider_id: SttProviderId,
    ) -> Option<Box<dyn SttProvider + Send + Sync>> {
        match provider_id {
            SttProviderId::Whisper => {
                let key = super::whisper_api_key(cfg);
                let provider = WhisperStt::with_options(
                    key,
                    resolve_openai_whisper_base_url(cfg),
                    cfg.voice.stt.whisper.model.clone(),
                    cfg.voice.stt.whisper.language.clone(),
                );
                if provider.is_configured() {
                    Some(Box::new(provider) as Box<dyn SttProvider + Send + Sync>)
                } else {
                    None
                }
            },
            SttProviderId::Deepgram => cfg.voice.stt.deepgram.api_key.clone().map(|key| {
                Box::new(DeepgramStt::with_options(
                    Some(key),
                    cfg.voice.stt.deepgram.model.clone(),
                    cfg.voice.stt.deepgram.language.clone(),
                    cfg.voice.stt.deepgram.smart_format,
                )) as Box<dyn SttProvider + Send + Sync>
            }),
            SttProviderId::Google => cfg.voice.stt.google.api_key.clone().map(|key| {
                Box::new(GoogleStt::with_options(
                    Some(key),
                    cfg.voice.stt.google.language.clone(),
                    cfg.voice.stt.google.model.clone(),
                )) as Box<dyn SttProvider + Send + Sync>
            }),
            SttProviderId::VoxtralLocal => {
                let provider = VoxtralLocalStt::with_options(
                    Some(cfg.voice.stt.voxtral_local.endpoint.clone()),
                    cfg.voice.stt.voxtral_local.model.clone(),
                    cfg.voice.stt.voxtral_local.language.clone(),
                );
                if provider.is_configured() {
                    Some(Box::new(provider) as Box<dyn SttProvider + Send + Sync>)
                } else {
                    None
                }
            },
            SttProviderId::WhisperLocal => {
                let provider = WhisperLocalStt::with_options(
                    Some(cfg.voice.stt.whisper_local.endpoint.clone()),
                    cfg.voice.stt.whisper_local.model.clone(),
                    cfg.voice.stt.whisper_local.language.clone(),
                );
                if provider.is_configured() {
                    Some(Box::new(provider) as Box<dyn SttProvider + Send + Sync>)
                } else {
                    None
                }
            },
            SttProviderId::WhisperCli => {
                let provider = WhisperCliStt::with_options(
                    cfg.voice.stt.whisper_cli.binary_path.clone(),
                    cfg.voice.stt.whisper_cli.model_path.clone(),
                    cfg.voice.stt.whisper_cli.language.clone(),
                );
                if provider.is_configured() {
                    Some(Box::new(provider) as Box<dyn SttProvider + Send + Sync>)
                } else {
                    None
                }
            },
            SttProviderId::SherpaOnnx => {
                let provider = SherpaOnnxStt::with_options(
                    cfg.voice.stt.sherpa_onnx.binary_path.clone(),
                    cfg.voice.stt.sherpa_onnx.model_dir.clone(),
                    cfg.voice.stt.sherpa_onnx.language.clone(),
                );
                if provider.is_configured() {
                    Some(Box::new(provider) as Box<dyn SttProvider + Send + Sync>)
                } else {
                    None
                }
            },
            SttProviderId::ElevenLabs => cfg
                .voice
                .stt
                .elevenlabs
                .api_key
                .clone()
                .or_else(|| cfg.voice.tts.elevenlabs.api_key.clone())
                .map(|key| {
                    Box::new(ElevenLabsStt::with_options(
                        Some(key),
                        cfg.voice.stt.elevenlabs.model.clone(),
                        cfg.voice.stt.elevenlabs.language.clone(),
                    )) as Box<dyn SttProvider + Send + Sync>
                }),
        }
    }

    /// List all providers with their configuration status from `chelix.toml`.
    fn list_providers(cfg: &chelix_config::ChelixConfig) -> Vec<(SttProviderId, bool)> {
        vec![
            (
                SttProviderId::Whisper,
                super::whisper_api_key(cfg).is_some()
                    || resolve_openai_whisper_base_url(cfg).is_some(),
            ),
            (
                SttProviderId::Deepgram,
                cfg.voice.stt.deepgram.api_key.is_some(),
            ),
            (
                SttProviderId::Google,
                cfg.voice.stt.google.api_key.is_some(),
            ),
            (
                SttProviderId::VoxtralLocal,
                VoxtralLocalStt::with_options(
                    Some(cfg.voice.stt.voxtral_local.endpoint.clone()),
                    cfg.voice.stt.voxtral_local.model.clone(),
                    cfg.voice.stt.voxtral_local.language.clone(),
                )
                .is_configured(),
            ),
            (
                SttProviderId::WhisperLocal,
                WhisperLocalStt::with_options(
                    Some(cfg.voice.stt.whisper_local.endpoint.clone()),
                    cfg.voice.stt.whisper_local.model.clone(),
                    cfg.voice.stt.whisper_local.language.clone(),
                )
                .is_configured(),
            ),
            (
                SttProviderId::WhisperCli,
                cfg.voice.stt.whisper_cli.model_path.is_some(),
            ),
            (
                SttProviderId::SherpaOnnx,
                cfg.voice.stt.sherpa_onnx.model_dir.is_some(),
            ),
            (
                SttProviderId::ElevenLabs,
                cfg.voice.stt.elevenlabs.api_key.is_some()
                    || cfg.voice.tts.elevenlabs.api_key.is_some(),
            ),
        ]
    }

    /// Resolve the active provider: explicit config value, or first configured.
    fn resolve_provider(
        cfg: &chelix_config::ChelixConfig,
        config_provider: Option<chelix_config::VoiceSttProvider>,
    ) -> Option<SttProviderId> {
        if let Some(p) = config_provider {
            return SttProviderId::parse(p.as_str());
        }
        // Auto-select: first configured provider
        Self::list_providers(cfg)
            .into_iter()
            .find(|(_, configured)| *configured)
            .map(|(id, _)| id)
    }
}

#[async_trait]
impl SttService for LiveSttService {
    async fn status(&self) -> ServiceResult {
        let cfg = self.load_config().await?;
        let providers = Self::list_providers(&cfg);
        let any_configured = providers.iter().any(|(_, configured)| *configured);
        let resolved = Self::resolve_provider(&cfg, cfg.voice.stt.provider);

        Ok(json!({
            "enabled": any_configured,
            "provider": resolved.map(|p| p.to_string()).unwrap_or_default(),
            "configured": any_configured,
        }))
    }

    async fn providers(&self) -> ServiceResult {
        let cfg = self.load_config().await?;
        let providers: Vec<_> = Self::list_providers(&cfg)
            .into_iter()
            .map(|(id, configured)| {
                json!({
                    "id": id,  // Uses serde serialization for consistent IDs
                    "name": id.name(),
                    "configured": configured,
                })
            })
            .collect();

        Ok(json!(providers))
    }

    async fn transcribe(&self, params: Value) -> ServiceResult {
        let audio_base64 = params
            .get("audio")
            .and_then(|v| v.as_str())
            .ok_or("missing 'audio' parameter (base64-encoded)")?;

        let audio_data = base64::engine::general_purpose::STANDARD
            .decode(audio_base64)
            .map_err(|e| format!("invalid base64 audio: {}", e))?;

        let format_str = params
            .get("format")
            .and_then(|v| v.as_str())
            .unwrap_or("mp3");

        self.transcribe_bytes(
            audio_data.into(),
            format_str,
            params.get("provider").and_then(|v| v.as_str()),
            params.get("language").and_then(|v| v.as_str()),
            params.get("prompt").and_then(|v| v.as_str()),
        )
        .await
    }

    async fn transcribe_bytes(
        &self,
        audio: bytes::Bytes,
        format: &str,
        provider: Option<&str>,
        language: Option<&str>,
        prompt: Option<&str>,
    ) -> ServiceResult {
        let cfg = self.load_config().await?;
        let audio_len = audio.len();

        let provider_id = match provider {
            Some(s) => {
                SttProviderId::parse(s).ok_or_else(|| format!("unknown STT provider '{s}'"))?
            },
            None => Self::resolve_provider(&cfg, cfg.voice.stt.provider)
                .ok_or_else(|| "no STT provider configured".to_string())?,
        };

        let stt_provider: Box<dyn SttProvider + Send + Sync> =
            Self::create_provider(&cfg, provider_id)
                .ok_or_else(|| format!("STT provider '{}' not configured", provider_id))?;

        let request = TranscribeRequest {
            audio,
            format: AudioFormat::from_short_name(format),
            language: language.map(String::from),
            prompt: prompt.map(String::from),
        };

        debug!(
            provider = %provider_id,
            format,
            audio_bytes = audio_len,
            language = language.unwrap_or("auto"),
            has_prompt = prompt.is_some(),
            "STT transcription request"
        );

        let transcript = stt_provider.transcribe(request).await.map_err(|e| {
            warn!(
                provider = %provider_id,
                format,
                audio_bytes = audio_len,
                error = %e,
                "STT transcription failed"
            );
            format!("transcription failed: {}", e)
        })?;

        Ok(json!({
            "text": transcript.text,
            "language": transcript.language,
            "confidence": transcript.confidence,
            "durationSeconds": transcript.duration_seconds,
            "words": transcript.words,
        }))
    }

    async fn set_provider(&self, params: Value) -> ServiceResult {
        let provider_str = params
            .get("provider")
            .and_then(|v| v.as_str())
            .ok_or("missing 'provider' parameter")?;

        let provider_id = SttProviderId::parse(provider_str)
            .ok_or_else(|| format!("unknown STT provider '{}'", provider_str))?;

        let cfg = self.load_config().await?;
        if Self::create_provider(&cfg, provider_id).is_none() {
            return Err(format!("provider '{}' not configured", provider_id).into());
        }

        self.config_service()
            .update(move |cfg| {
                cfg.voice.stt.provider = Some(provider_id);
                Ok(())
            })
            .await
            .map_err(|e| format!("failed to update config: {e}"))?;

        debug!("STT provider set to: {}", provider_id);

        Ok(json!({
            "provider": provider_id,  // Uses serde serialization
        }))
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use {super::*, crate::services::NoopSttService, serde_json::json};

    use crate::test_config::ConfigFixture as VoiceConfigTestGuard;

    #[test]
    fn test_live_stt_resolve_provider_handles_explicit_and_auto_selection() {
        let config = chelix_config::ChelixConfig::default();
        assert_eq!(
            LiveSttService::resolve_provider(
                &config,
                Some(chelix_config::VoiceSttProvider::Whisper),
            ),
            Some(SttProviderId::Whisper)
        );
        assert!(LiveSttService::resolve_provider(&config, None).is_some());
    }

    #[tokio::test]
    async fn test_live_stt_whisper_base_url_counts_as_configured() {
        let guard = VoiceConfigTestGuard::with_config(
            r#"
[server]
port = 18080

[voice.stt.whisper]
base_url = "http://127.0.0.1:8001/"

[tools.execute_command]
terminal_size = "115x58"

[sandbox]
archived_session_retention_days = 3
"#,
        )
        .await;

        let config = load_voice_config(&guard.service)
            .await
            .expect("load voice config");
        let providers = LiveSttService::list_providers(&config);
        let whisper = providers
            .into_iter()
            .find(|(id, _)| *id == SttProviderId::Whisper);

        assert_eq!(whisper, Some((SttProviderId::Whisper, true)));
        // With explicit provider selection, Whisper is chosen
        assert_eq!(
            LiveSttService::resolve_provider(
                &config,
                Some(chelix_config::VoiceSttProvider::Whisper),
            ),
            Some(SttProviderId::Whisper)
        );
    }

    #[tokio::test]
    async fn test_live_stt_local_provider_config_status_uses_provider_rules() {
        let guard = VoiceConfigTestGuard::with_config(
            r#"
[server]
port = 18080

[voice.stt.voxtral_local]
endpoint = "http://127.0.0.1:9000/"

[voice.stt.whisper_local]
endpoint = "http://127.0.0.1:9001/"

[tools.execute_command]
terminal_size = "115x58"

[sandbox]
archived_session_retention_days = 3
"#,
        )
        .await;

        let config = load_voice_config(&guard.service)
            .await
            .expect("load voice config");
        let providers = LiveSttService::list_providers(&config);
        let voxtral_local = providers
            .iter()
            .find(|(id, _)| *id == SttProviderId::VoxtralLocal);
        let whisper_local = providers
            .iter()
            .find(|(id, _)| *id == SttProviderId::WhisperLocal);

        assert_eq!(voxtral_local, Some(&(SttProviderId::VoxtralLocal, true)));
        assert_eq!(whisper_local, Some(&(SttProviderId::WhisperLocal, true)));
    }

    #[tokio::test]
    async fn test_live_stt_service_status() {
        let guard = VoiceConfigTestGuard::with_config(
            "[tools.execute_command]\nterminal_size = \"115x58\"\n\n[sandbox]\narchived_session_retention_days = 3\n",
        )
        .await;
        let service = LiveSttService::new(guard.service.clone());
        let status = service.status().await.unwrap();

        // Status should always contain these fields
        assert!(status.get("enabled").is_some());
        assert!(status.get("configured").is_some());
        assert!(status.get("provider").is_some());
        // voxtral-local is always considered "configured" (local service)
        // so configured will be true even with no API keys
        assert_eq!(status["configured"], true);
    }

    #[tokio::test]
    async fn test_live_stt_service_providers() {
        let guard = VoiceConfigTestGuard::with_config(
            "[tools.execute_command]\nterminal_size = \"115x58\"\n\n[sandbox]\narchived_session_retention_days = 3\n",
        )
        .await;
        let service = LiveSttService::new(guard.service.clone());
        let providers = service.providers().await.unwrap();

        let providers_arr = providers.as_array().unwrap();
        // 4 cloud providers + 4 local providers.
        assert_eq!(providers_arr.len(), 8);
        // Check all providers are listed
        let ids: Vec<_> = providers_arr
            .iter()
            .filter_map(|p| p["id"].as_str())
            .collect();
        assert!(ids.contains(&"whisper"));
        assert!(ids.contains(&"deepgram"));
        assert!(ids.contains(&"google"));
        assert!(ids.contains(&"voxtral-local"));
        assert!(ids.contains(&"whisper-local"));
        assert!(ids.contains(&"whisper-cli"));
        assert!(ids.contains(&"sherpa-onnx"));
        assert!(ids.contains(&"elevenlabs-stt"));
    }

    #[tokio::test]
    async fn test_noop_stt_service() {
        let service = NoopSttService;

        let status = service.status().await.unwrap();
        assert_eq!(status["enabled"], false);

        let providers = service.providers().await.unwrap();
        assert_eq!(providers.as_array().unwrap().len(), 0);

        let result = service.transcribe(json!({})).await;
        assert!(result.is_err());

        let result = service
            .transcribe_bytes(bytes::Bytes::from_static(b"fake"), "mp3", None, None, None)
            .await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), "STT not available");
    }
}
