use serde::{Deserialize, Deserializer, de};

pub use chelix_call_bus::{SkillContent, SkillMetadata, SkillOrigin, SkillSource};

/// Agent list: a missing value, null, or `[]` is empty. Any other shape is an error.
pub(crate) fn deserialize_agent_list<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_yaml::Value::deserialize(deserializer)?;
    match value {
        serde_yaml::Value::Null => Ok(Vec::new()),
        serde_yaml::Value::Sequence(items) => items
            .into_iter()
            .map(|item| match item {
                serde_yaml::Value::String(id) => Ok(id),
                _ => Err(de::Error::custom("agent id must be a string")),
            })
            .collect(),
        _ => Err(de::Error::custom(
            "agent list must be a sequence of strings",
        )),
    }
}
