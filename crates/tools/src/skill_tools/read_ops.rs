#[cfg(feature = "metrics")]
use chelix_metrics::{counter, labels, skills as skills_metrics};
use {
    crate::error::Error,
    async_trait::async_trait,
    chelix_agents::{tool_context::ToolExecutionContext, tool_registry::AgentTool},
    chelix_call_bus::{CallBus, ReadSkillFile, SkillActor, SkillLocate, SkillRead, SkillSource},
    chelix_skills::usage::SkillUsageStore,
    serde_json::{Value, json},
    std::{path::Path, sync::Arc},
};

pub struct ReadSkillTool {
    bus: Arc<CallBus>,
    usage_store: Option<SkillUsageStore>,
}

impl ReadSkillTool {
    #[must_use]
    pub fn new(bus: Arc<CallBus>) -> Self {
        Self {
            bus,
            usage_store: None,
        }
    }

    #[must_use]
    pub fn with_usage_store(mut self, store: SkillUsageStore) -> Self {
        self.usage_store = Some(store);
        self
    }
}

#[async_trait]
impl AgentTool for ReadSkillTool {
    fn name(&self) -> &str {
        "read_skill"
    }

    fn description(&self) -> &str {
        "Load a skill's instructions and linked resources."
    }

    fn parameters_schema(&self) -> Value {
        json!({ "type": "object", "required": ["name"], "properties": {
            "name": { "type": "string", "description": "Skill name (use the names from <available_skills>)" },
            "file_path": { "type": "string", "description": "Optional relative path to a linked file within the skill, including nested paths (for example, 'references/api.md'). Omit to read the main SKILL.md body." }
        }})
    }

    async fn execute(&self, _params: Value) -> anyhow::Result<Value> {
        Err(Error::message("missing agent id").into())
    }

    async fn execute_with_context(
        &self,
        params: Value,
        context: &ToolExecutionContext,
    ) -> anyhow::Result<Value> {
        let id = super::helpers::agent_id(context)?;
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::message("missing 'name'"))?;
        let file_path = params
            .get("file_path")
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
            .map(str::to_owned);
        let content = self
            .bus
            .call(ReadSkillFile {
                name: name.into(),
                max_bytes: Some(super::MAX_SKILL_BODY_BYTES as u64),
                actor: SkillActor::Agent { id: id.into() },
                locate: SkillLocate::Discovered,
                file_path,
            })
            .await?;
        let primary = content.sidecar.is_none();
        let response = read_response(name, content);
        if primary {
            if let Some(store) = &self.usage_store {
                store.record_read(name).await;
            }
            #[cfg(feature = "metrics")]
            counter!(skills_metrics::ACTIVATIONS_TOTAL, labels::TOOL => "read_skill".to_string())
                .increment(1);
        }
        Ok(response)
    }
}

fn read_response(name: &str, content: SkillRead) -> Value {
    let bundled = content.metadata.source == Some(SkillSource::Bundled);
    if let Some(sidecar) = content.sidecar {
        if let Some(text) = sidecar.text {
            return json!({ "name": name, "file_path": sidecar.path, "bytes": sidecar.bytes, "content": text, "is_binary": false });
        }
        let mut response = json!({ "name": name, "file_path": sidecar.path, "bytes": sidecar.bytes, "is_binary": true });
        if bundled {
            response["note"] = json!(format!(
                "Binary file ({} bytes). Contents omitted.",
                sidecar.bytes
            ));
        } else {
            let file_type = Path::new(&sidecar.path)
                .extension()
                .and_then(|extension| extension.to_str())
                .map(|extension| format!(".{extension}"))
                .unwrap_or_default();
            response["file_type"] = json!(file_type);
            response["note"] = json!(format!(
                "Binary file ({} bytes). Contents omitted — the model cannot consume binary data directly.",
                sidecar.bytes
            ));
        }
        return response;
    }
    let meta = content.metadata;
    if !bundled {
        let hits = chelix_skills::safety::scan_skill_body(name, &content.body);
        if !hits.is_empty() {
            tracing::warn!(skill = %name, patterns = ?hits, "skill body contains potential prompt-injection patterns");
        }
    }
    let source = match meta.source {
        Some(SkillSource::Project) => "project",
        Some(SkillSource::Personal) => "personal",
        Some(SkillSource::Bundled) => "bundled",
        None => "unknown",
    };
    let mut response = json!({ "name": name, "description": meta.description, "source": source, "body": content.body, "bytes": content.body.len(), "linked_files": content.linked_files.iter().map(|file| json!({ "path": file.path, "bytes": file.bytes })).collect::<Vec<_>>() });
    if let Some(dir_name) = content.skill_dir_name {
        response["skill_dir_name"] = json!(dir_name);
    }
    if let Some(dir) = &content.skill_dir {
        response["skill_dir"] = json!(dir.display().to_string());
    }
    if let Some(name) = meta.display_name {
        response["display_name"] = json!(name);
    }
    if let Some(license) = meta.license {
        response["license"] = json!(license);
    }
    if let Some(homepage) = meta.homepage {
        response["homepage"] = json!(homepage);
    }
    if bundled {
        if let Some(category) = meta.category {
            response["category"] = json!(category);
        }
        if let Some(origin) = meta.origin {
            response["origin"] = json!(origin);
        }
    } else if let Some(compatibility) = meta.compatibility {
        response["compatibility"] = json!(compatibility);
    }
    if !content.linked_files.is_empty() {
        let hint = if bundled {
            if content.skill_dir.is_some()
                && content
                    .linked_files
                    .iter()
                    .any(|file| file.path.starts_with("scripts/"))
            {
                "This skill includes executable scripts. Use the `skill_dir` path to resolve script references in the body. To view a linked file, call read_skill again with file_path set to one of the paths in linked_files."
            } else {
                "To view a linked file, call read_skill again with file_path set to one of the paths in linked_files."
            }
        } else {
            "To view a linked file, call read_skill again with file_path set to one of the paths in linked_files (e.g. file_path=\"references/api.md\"). Nested paths inside those directories are also supported."
        };
        response["usage_hint"] = json!(hint);
    }
    response
}
