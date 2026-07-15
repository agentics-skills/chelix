//! Provider registry: model registration, lookup, discovery, and lifecycle.

mod core;
mod discovery;
pub mod registration;
#[cfg(test)]
mod tests;

pub use self::{
    core::*,
    discovery::{DiscoveryResult, discover_models},
};
