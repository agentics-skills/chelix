//! Transport-neutral message media type.

use serde::{Deserialize, Serialize};

/// Input and requested reply media used by one user message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageMedium {
    Text,
    Voice,
}
