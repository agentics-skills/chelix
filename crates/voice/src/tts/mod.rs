//! Text-to-Speech provider abstraction and implementations.

mod types;
pub use types::{AudioFormat, AudioOutput, SynthesizeRequest, TtsProvider, Voice};

#[cfg(feature = "runtime")]
mod audio;
#[cfg(feature = "runtime")]
mod coqui;
#[cfg(feature = "runtime")]
mod elevenlabs;
#[cfg(feature = "runtime")]
mod google;
#[cfg(feature = "runtime")]
pub mod mock;
#[cfg(feature = "runtime")]
mod openai;
#[cfg(feature = "runtime")]
mod piper;

#[cfg(feature = "runtime")]
pub use {
    coqui::CoquiTts, elevenlabs::ElevenLabsTts, google::GoogleTts, mock::MockTts,
    openai::OpenAiTts, piper::PiperTts,
};
