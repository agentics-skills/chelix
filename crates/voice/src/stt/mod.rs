//! Speech-to-Text provider abstraction and implementations.

mod types;
pub use types::{SttProvider, TranscribeRequest, Transcript, Word};

#[cfg(feature = "runtime")]
mod cli_utils;
#[cfg(feature = "runtime")]
mod deepgram;
#[cfg(feature = "runtime")]
mod elevenlabs;
#[cfg(feature = "runtime")]
mod google;
#[cfg(feature = "runtime")]
pub mod mock;
#[cfg(feature = "runtime")]
mod openai_compat;
#[cfg(feature = "runtime")]
mod sherpa_onnx;
#[cfg(feature = "runtime")]
mod voxtral_local;
#[cfg(feature = "runtime")]
mod whisper;
#[cfg(feature = "runtime")]
mod whisper_cli;
#[cfg(feature = "runtime")]
mod whisper_local;

#[cfg(feature = "runtime")]
pub use {
    deepgram::DeepgramStt, elevenlabs::ElevenLabsStt, google::GoogleStt, mock::MockStt,
    sherpa_onnx::SherpaOnnxStt, voxtral_local::VoxtralLocalStt, whisper::WhisperStt,
    whisper_cli::WhisperCliStt, whisper_local::WhisperLocalStt,
};
