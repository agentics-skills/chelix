//! Provider registry: atomic config-only registration and lookup.

mod core;
pub mod registration;
#[cfg(test)]
mod tests;

pub use self::core::*;
