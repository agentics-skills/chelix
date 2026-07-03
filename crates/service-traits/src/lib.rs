//! Service trait interfaces for domain services.
//!
//! Each trait has a `Noop` implementation that returns empty/default responses,
//! allowing the gateway to run standalone before domain crates are wired in.

mod bundle;
mod error;
mod interfaces;
mod session_mutations;

pub use crate::{
    bundle::Services,
    error::{ServiceError, ServiceResult},
    interfaces::*,
    session_mutations::{SessionBusyReason, SessionMutationCoordinator},
};

#[cfg(test)]
mod tests;
