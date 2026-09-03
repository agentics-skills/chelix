//! Service trait interfaces for domain services.
//!
//! Each trait has a `Noop` implementation that returns empty/default responses,
//! allowing the gateway to run standalone before domain crates are wired in.

mod bundle;
mod chat_request;
mod error;
mod interfaces;
mod session_mutations;

pub use crate::{
    bundle::Services,
    chat_request::{
        ChatChannelMetadata, ChatExecutionContext, ChatRequestOrigin, ChatSendDocument,
        ChatSendMessage, ChatSendRequest, ChatSendSyncRequest,
    },
    error::{ServiceError, ServiceResult},
    interfaces::*,
    session_mutations::{SessionBusyReason, SessionMutationCoordinator, SessionTurnPermit},
};

#[cfg(test)]
mod tests;
