//! Interactive onboarding wizard.
//!
//! Flow: welcome → user name → agent name → agent emoji → confirm → done.

#[cfg(feature = "metrics")]
use chelix_metrics as _;

pub mod error;
pub mod service;
pub mod state;
pub mod wizard;

pub use error::{Context, Error, Result};
