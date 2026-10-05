//! `LiveProviderSetupService` — the runtime implementation of
//! `ProviderSetupService` that manages provider credentials and registry rebuilds.

#[path = "../support.rs"]
mod support;

mod available;
mod credentials;
mod openai_compatible;
mod provider_models;
mod service;

pub use service::*;

#[cfg(test)]
#[path = "../tests.rs"]
mod tests;
