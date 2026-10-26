//! Queued configuration transactions and snapshot reads.

mod document;
mod error;
mod initialization;
mod reader;
mod refresh;
mod service;
mod snapshot;
mod storage;
mod validation;
mod writer;

pub use {
    error::{Error, Result},
    initialization::{ConfigInitialization, ConfigInitializer, ConfigOrigin},
    service::ConfigService,
    snapshot::ConfigDocument,
    validation::{BlockingValidator, ConfigValidation, ConfigValidator, SyncValidator},
    writer::PersistedConfigStatus,
};

#[cfg(test)]
mod tests;
