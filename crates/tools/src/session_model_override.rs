//! Shared model override parsing for session tools.

use {
    chelix_common::ConfigModelOverride,
    serde::{Deserialize, Deserializer, de::Error as _},
};

pub use chelix_common::ModelOverride;

pub(crate) fn deserialize_model_override<'de, D>(
    deserializer: D,
) -> Result<Option<ModelOverride>, D::Error>
where
    D: Deserializer<'de>,
{
    let model_override = ConfigModelOverride::deserialize(deserializer)?;
    if model_override.model.is_empty() || model_override.reasoning_effort.as_str().is_empty() {
        return Err(D::Error::custom(
            "model_override requires non-empty model and reasoning_effort",
        ));
    }
    Ok(Some(model_override.into()))
}
