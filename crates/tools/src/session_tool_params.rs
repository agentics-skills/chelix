//! Shared string deserializers for closed session-tool parameters.

use serde::{Deserialize, Deserializer, de::Error as _};

pub(crate) fn nonempty_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    if value.is_empty() {
        return Err(D::Error::custom("string must not be empty"));
    }
    Ok(value)
}

pub(crate) fn optional_nonempty_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    nonempty_string(deserializer).map(Some)
}
