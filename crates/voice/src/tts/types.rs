use {
    anyhow::Result,
    async_trait::async_trait,
    bytes::Bytes,
    serde::{Deserialize, Serialize},
};

/// A voice available from a TTS provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Voice {
    /// Provider-specific voice identifier.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Optional description or tags.
    pub description: Option<String>,
    /// Preview URL if available.
    pub preview_url: Option<String>,
}

/// Audio output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioFormat {
    /// MP3 format (widely compatible).
    #[default]
    Mp3,
    /// Opus in OGG container (good for Telegram voice notes).
    Opus,
    /// AAC format.
    Aac,
    /// PCM (raw audio).
    Pcm,
    /// WAV container with PCM audio.
    Wav,
    /// WebM container (browser MediaRecorder default).
    Webm,
}

impl AudioFormat {
    /// MIME type for this format.
    #[must_use]
    pub fn mime_type(&self) -> &'static str {
        match self {
            Self::Mp3 => "audio/mpeg",
            Self::Opus => "audio/ogg",
            Self::Aac => "audio/aac",
            Self::Pcm => "audio/pcm",
            Self::Wav => "audio/wav",
            Self::Webm => "audio/webm",
        }
    }

    /// File extension for this format.
    #[must_use]
    pub fn extension(&self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::Opus => "ogg",
            Self::Aac => "aac",
            Self::Pcm => "pcm",
            Self::Wav => "wav",
            Self::Webm => "webm",
        }
    }

    /// Parse from a MIME content-type string (e.g. `"audio/webm"`, `"audio/webm;codecs=opus"`).
    #[must_use]
    pub fn from_content_type(ct: &str) -> Option<Self> {
        let base = ct.split(';').next().unwrap_or(ct).trim();
        match base {
            "audio/webm" => Some(Self::Webm),
            "audio/ogg" | "audio/opus" => Some(Self::Opus),
            "audio/mpeg" | "audio/mp3" => Some(Self::Mp3),
            "audio/aac" | "audio/mp4" | "audio/m4a" => Some(Self::Aac),
            "audio/pcm" => Some(Self::Pcm),
            "audio/wav" | "audio/x-wav" => Some(Self::Wav),
            _ => None,
        }
    }

    /// Parse from a short format name (e.g. `"webm"`, `"opus"`, `"ogg"`, `"mp3"`).
    #[must_use]
    pub fn from_short_name(name: &str) -> Self {
        match name {
            "webm" => Self::Webm,
            "opus" | "ogg" => Self::Opus,
            "aac" | "m4a" => Self::Aac,
            "pcm" => Self::Pcm,
            "wav" => Self::Wav,
            _ => Self::Mp3,
        }
    }
}

/// Request to synthesize speech from text.
#[derive(Debug, Clone, Default)]
pub struct SynthesizeRequest {
    /// Text to convert to speech.
    pub text: String,
    /// Voice ID (provider-specific).
    pub voice_id: Option<String>,
    /// Model to use (provider-specific).
    pub model: Option<String>,
    /// Output audio format.
    pub output_format: AudioFormat,
    /// Speed multiplier (0.5 - 2.0).
    pub speed: Option<f32>,
    /// Stability setting (ElevenLabs-specific, 0.0 - 1.0).
    pub stability: Option<f32>,
    /// Similarity boost (ElevenLabs-specific, 0.0 - 1.0).
    pub similarity_boost: Option<f32>,
    /// Voice persona instructions injected into providers that support them.
    ///
    /// OpenAI `gpt-4o-mini-tts` uses this as the `instructions` field to control
    /// voice tone, accent, pacing and character. Other providers ignore it or
    /// map it to provider-specific mechanisms.
    pub instructions: Option<String>,
}

/// Audio output from TTS synthesis.
#[derive(Debug, Clone)]
pub struct AudioOutput {
    /// Raw audio data.
    pub data: Bytes,
    /// Audio format.
    pub format: AudioFormat,
    /// Duration in milliseconds (if known).
    pub duration_ms: Option<u64>,
}

/// Text-to-Speech provider trait.
///
/// Implementations provide speech synthesis from text using various services.
#[async_trait]
pub trait TtsProvider: Send + Sync {
    /// Provider identifier (e.g., "elevenlabs", "openai").
    fn id(&self) -> &'static str;

    /// Human-readable provider name.
    fn name(&self) -> &'static str;

    /// Check if the provider is configured and ready.
    fn is_configured(&self) -> bool;

    /// Whether this provider supports SSML tags natively.
    ///
    /// Providers that return `true` will receive SSML-tagged text as-is.
    /// Providers that return `false` will have SSML tags stripped before
    /// synthesis (handled centrally in the gateway's `convert()` handler).
    fn supports_ssml(&self) -> bool {
        false
    }

    /// List available voices from this provider.
    async fn voices(&self) -> Result<Vec<Voice>>;

    /// Convert text to speech.
    async fn synthesize(&self, request: SynthesizeRequest) -> Result<AudioOutput>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_format_mime_type() {
        assert_eq!(AudioFormat::Mp3.mime_type(), "audio/mpeg");
        assert_eq!(AudioFormat::Opus.mime_type(), "audio/ogg");
        assert_eq!(AudioFormat::Webm.mime_type(), "audio/webm");
        assert_eq!(AudioFormat::Aac.mime_type(), "audio/aac");
        assert_eq!(AudioFormat::Pcm.mime_type(), "audio/pcm");
        assert_eq!(AudioFormat::Wav.mime_type(), "audio/wav");
    }

    #[test]
    fn test_audio_format_extension() {
        assert_eq!(AudioFormat::Mp3.extension(), "mp3");
        assert_eq!(AudioFormat::Opus.extension(), "ogg");
        assert_eq!(AudioFormat::Webm.extension(), "webm");
        assert_eq!(AudioFormat::Aac.extension(), "aac");
        assert_eq!(AudioFormat::Pcm.extension(), "pcm");
        assert_eq!(AudioFormat::Wav.extension(), "wav");
    }

    #[test]
    fn test_audio_format_from_content_type() {
        assert_eq!(
            AudioFormat::from_content_type("audio/webm"),
            Some(AudioFormat::Webm)
        );
        assert_eq!(
            AudioFormat::from_content_type("audio/webm;codecs=opus"),
            Some(AudioFormat::Webm)
        );
        assert_eq!(
            AudioFormat::from_content_type("audio/ogg"),
            Some(AudioFormat::Opus)
        );
        assert_eq!(
            AudioFormat::from_content_type("audio/mpeg"),
            Some(AudioFormat::Mp3)
        );
        assert_eq!(
            AudioFormat::from_content_type("audio/mp3"),
            Some(AudioFormat::Mp3)
        );
        assert_eq!(
            AudioFormat::from_content_type("audio/aac"),
            Some(AudioFormat::Aac)
        );
        assert_eq!(
            AudioFormat::from_content_type("audio/mp4"),
            Some(AudioFormat::Aac)
        );
        assert_eq!(
            AudioFormat::from_content_type("audio/pcm"),
            Some(AudioFormat::Pcm)
        );
        assert_eq!(
            AudioFormat::from_content_type("audio/wav"),
            Some(AudioFormat::Wav)
        );
        assert_eq!(
            AudioFormat::from_content_type("audio/x-wav"),
            Some(AudioFormat::Wav)
        );
        assert_eq!(AudioFormat::from_content_type("image/png"), None);
        assert_eq!(AudioFormat::from_content_type("text/plain"), None);
    }

    #[test]
    fn test_audio_format_from_short_name() {
        assert_eq!(AudioFormat::from_short_name("webm"), AudioFormat::Webm);
        assert_eq!(AudioFormat::from_short_name("opus"), AudioFormat::Opus);
        assert_eq!(AudioFormat::from_short_name("ogg"), AudioFormat::Opus);
        assert_eq!(AudioFormat::from_short_name("aac"), AudioFormat::Aac);
        assert_eq!(AudioFormat::from_short_name("pcm"), AudioFormat::Pcm);
        assert_eq!(AudioFormat::from_short_name("wav"), AudioFormat::Wav);
        assert_eq!(AudioFormat::from_short_name("mp3"), AudioFormat::Mp3);
        assert_eq!(AudioFormat::from_short_name("unknown"), AudioFormat::Mp3);
    }

    #[test]
    fn test_synthesize_request_default() {
        let req = SynthesizeRequest::default();
        assert!(req.text.is_empty());
        assert_eq!(req.output_format, AudioFormat::Mp3);
    }
}
