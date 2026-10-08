//! In-process registry of typed calls and events.

mod bus;
mod error;
mod procedure;

pub use {
    bus::{CallBus, PublishOutcome, QUEUE_CAPACITY},
    error::{CallError, PublishError, RegError},
    procedure::{Delivery, Event, Procedure},
};

#[cfg(test)]
mod tests;
