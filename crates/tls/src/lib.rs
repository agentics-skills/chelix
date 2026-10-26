//! TLS certificate management and HTTPS server support.
//!
//! On first run, generates a local CA and server certificate (mkcert-style)
//! so the gateway can serve HTTPS out of the box. A companion plain-HTTP
//! server on a secondary port serves the CA cert for easy download and
//! redirects everything else to HTTPS.

#[cfg(feature = "runtime")]
mod certs;
pub mod config_validation;
#[cfg(feature = "runtime")]
pub mod error;
#[cfg(feature = "runtime")]
mod server;

#[cfg(feature = "runtime")]
pub use {
    certs::*,
    error::{Context, Error, Result},
    server::*,
};
