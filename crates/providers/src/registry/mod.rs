//! Provider registry: atomic config-only registration and lookup.

mod core;
pub mod registration;
#[cfg(test)]
mod tests;

pub use self::core::*;

pub type ResolvedModelReasoning = chelix_common::ResolvedModelReasoning;
