//! Route inbound messages to agents and build session keys.
//!
//! Binding cascade (precedence):
//! 1. Peer binding (exact peer ID match)
//! 2. Account binding (channel + account)
//! 3. Channel binding (channel + wildcard account)
//! 4. Default agent (agents.defaults.id)

pub mod error;
pub mod resolve;

pub use {
    error::{Error, Result},
    resolve::resolve_agent_route,
};
