//! Shared model override parsing for session tools.

use {
    chelix_common::ConfigModelOverride,
    serde::{Deserialize, Deserializer, de::Error as _},
    serde_json::Value,
};

pub use chelix_common::ModelOverride;

pub fn model_override_schema() -> Value {
    serde_json::json!({
        "description": "Advanced use only. Omit to use the selected agent's preset model. Provide this only when intentionally overriding the agent's preset with a different model configuration. Do not copy preset model values returned by sessions_explore.",
        "type": "object",
        "additionalProperties": false,
        "required": ["model", "reasoning_effort"],
        "properties": {
            "model": {
                "description": "Base model id override from the chat model registry. Must be different from the selected agent's preset model. Do not pass null or empty strings.",
                "minLength": 1,
                "type": "string"
            },
            "reasoning_effort": {
                "description": "Exact reasoning effort advertised by the selected model's reasoning_supported_efforts metadata. Required inside model_override. Do not pass null or empty strings.",
                "minLength": 1,
                "type": "string"
            }
        }
    })
}

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
