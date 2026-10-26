//! Voice capabilities for chelix: Text-to-Speech (TTS) and Speech-to-Text (STT).
//!
//! This crate provides provider-agnostic abstractions for voice services,
//! with implementations for popular providers like ElevenLabs, OpenAI, and Whisper.

pub mod config;
pub mod config_validation;
pub mod stt;
pub mod text;
pub mod tts;

pub use {
    config::{
        CoquiTtsConfig, DeepgramConfig, ElevenLabsConfig, ElevenLabsSttConfig, FallbackPolicy,
        GoogleSttConfig, GoogleTtsConfig, OpenAiTtsConfig, PiperTtsConfig, SherpaOnnxConfig,
        SttConfig, SttProviderId, TtsAutoMode, TtsConfig, TtsProviderId, VoiceConfig, VoicePersona,
        VoicePersonaPrompt, VoicePersonaProviderBinding, VoxtralLocalConfig, WhisperCliConfig,
        WhisperConfig, WhisperLocalConfig,
    },
    stt::{SttProvider, TranscribeRequest, Transcript},
    text::{
        TtsDirectives, contains_ssml, parse_tts_directives, sanitize_text_for_tts, strip_ssml_tags,
    },
    tts::{AudioFormat, AudioOutput, SynthesizeRequest, TtsProvider, Voice},
};

#[cfg(feature = "runtime")]
pub use {
    stt::{
        DeepgramStt, ElevenLabsStt, GoogleStt, SherpaOnnxStt, VoxtralLocalStt, WhisperCliStt,
        WhisperLocalStt, WhisperStt,
    },
    tts::{CoquiTts, ElevenLabsTts, GoogleTts, OpenAiTts, PiperTts},
};
