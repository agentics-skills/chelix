//! Shared model override parsing for session tools.

use {chelix_config::schema::ReasoningEffort, serde_json::Value};

use crate::{Error, Result, params::str_param};

#[derive(Debug, Clone)]
pub struct ModelOverride {
    pub model: String,
    pub reasoning_effort: ReasoningEffort,
}

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
                "description": "Reasoning effort to use with the model override. Required inside model_override. Do not pass null or empty strings.",
                "enum": ["none", "minimal", "low", "medium", "high", "xhigh", "max"],
                "type": "string"
            }
        }
    })
}

pub fn parse_model_override(params: &Value) -> Result<Option<ModelOverride>> {
    let Some(value) = params.get("model_override") else {
        return Ok(None);
    };
    let object = value.as_object().ok_or_else(|| {
        Error::message("model_override must be an object; omit the field instead of passing null")
    })?;
    for key in object.keys() {
        if key != "model" && key != "reasoning_effort" {
            return Err(Error::message(format!(
                "unsupported model_override field '{key}'; expected only model and reasoning_effort"
            )));
        }
    }
    let model = str_param(value, "model")
        .ok_or_else(|| Error::message("model_override.model must be a non-empty string"))?
        .to_string();
    let reasoning_effort = str_param(value, "reasoning_effort")
        .ok_or_else(|| Error::message("model_override.reasoning_effort must be a non-empty string"))
        .and_then(parse_reasoning_effort)?;
    Ok(Some(ModelOverride {
        model,
        reasoning_effort,
    }))
}

fn parse_reasoning_effort(value: &str) -> Result<ReasoningEffort> {
    ReasoningEffort::try_from(value).map_err(|error| {
        Error::message(format!(
            "invalid reasoning_effort '{value}'; {error}; expected one of: {}",
            ReasoningEffort::ALL
                .iter()
                .map(|effort| effort.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    })
}
