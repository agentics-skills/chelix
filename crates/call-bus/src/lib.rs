//! In-process registry of typed calls and events.

mod bus;
mod error;
mod procedure;
mod skill_file;
mod stop_session;

pub use {skill_file::*, stop_session::*};

pub use {
    bus::{CallBus, PublishOutcome, QUEUE_CAPACITY},
    error::{CallError, PublishError, RegError},
    procedure::{Delivery, Event, Procedure},
};

#[cfg(test)]
mod tests;
