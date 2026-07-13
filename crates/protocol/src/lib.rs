//! Chelix wire protocol definitions.
//!
//! Protocol version 4 (backward-compatible with v3). All communication uses JSON frames over WebSocket.
//!
//! Frame types:
//! - `RequestFrame`  — client → gateway RPC call (also server → client in v4)
//! - `ResponseFrame` — gateway → client RPC result (also client → server in v4)
//! - `EventFrame`    — gateway → client server-push

mod embedding;
mod types;
pub use {embedding::*, types::*};
