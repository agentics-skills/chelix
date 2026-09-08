pub(crate) mod core;
mod request;
mod streaming;
mod websocket;

#[cfg(test)]
mod options_tests;

// Re-export the struct so submodules can reach it via `super::OpenAiProvider`.
use super::OpenAiProvider;
