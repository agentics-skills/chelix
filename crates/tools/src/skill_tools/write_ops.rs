//! Write and patch skill files: sidecar writes and surgical find/replace.

use {
    chelix_call_bus::{SkillActor, SkillSidecarWrite, WriteSkillSidecars},
    std::sync::Arc,
};

use {
    async_trait::async_trait,
    chelix_agents::{tool_context::ToolExecutionContext, tool_registry::AgentTool},
    chelix_skills::usage::SkillUsageStore,
    serde_json::{Value, json},
};

#[cfg(feature = "metrics")]
use chelix_metrics::{counter, labels, skills as skills_metrics};

use {crate::error::Error, chelix_call_bus::MAX_SIDECAR_FILES_PER_CALL};

// ── WriteSkillFilesTool ─────────────────────────────────────

/// Tool that writes supplementary text files inside an existing personal skill.
pub struct WriteSkillFilesTool {
    bus: Arc<chelix_call_bus::CallBus>,
}

impl WriteSkillFilesTool {
    pub fn new(bus: Arc<chelix_call_bus::CallBus>) -> Self {
        Self { bus }
    }
}

#[async_trait]
impl AgentTool for WriteSkillFilesTool {
    fn name(&self) -> &str {
        "write_skill_files"
    }

    fn description(&self) -> &str {
        "Write supplementary UTF-8 text files inside an existing personal skill directory. \
         This tool is disabled by default and only appears when skills.enable_agent_sidecar_files is enabled."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["name", "files"],
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Existing skill name to update"
                },
                "files": {
                    "type": "array",
                    "description": "Supplementary text files to write inside the skill directory",
                    "minItems": 1,
                    "maxItems": MAX_SIDECAR_FILES_PER_CALL,
                    "items": {
                        "type": "object",
                        "required": ["path", "content"],
                        "properties": {
                            "path": {
                                "type": "string",
                                "description": "Relative path inside the skill directory"
                            },
                            "content": {
                                "type": "string",
                                "description": "UTF-8 text content to write"
                            }
                        }
                    }
                }
            }
        })
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
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::message("missing 'name'"))?;

        let files = params
            .get("files")
            .and_then(|v| v.as_array())
            .ok_or_else(|| Error::message("missing 'files'"))?;
        let files = files
            .iter()
            .map(|file| {
                let relative_path = file
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::message("each file needs a string 'path'"))?
                    .to_string();
                let content = file
                    .get("content")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::message("each file needs a string 'content'"))?
                    .to_string();
                Ok(SkillSidecarWrite {
                    relative_path,
                    content,
                })
            })
            .collect::<crate::Result<Vec<_>>>()?;
        let file_paths = files
            .iter()
            .map(|file| file.relative_path.clone())
            .collect::<Vec<_>>();
        let directory = self
            .bus
            .call(WriteSkillSidecars {
                name: name.into(),
                files,
                agent_id: id.into(),
            })
            .await
            .map_err(|error| super::helpers::missing_skill(name, error))?;

        Ok(json!({
            "written": true,
            "path": directory.path.display().to_string(),
            "files_written": file_paths.len(),
            "files": file_paths,
        }))
    }
}

// ── PatchSkillTool ──────────────────────────────────────────

/// Maximum number of patches per call.
const MAX_PATCHES_PER_CALL: usize = 10;

/// Tool that applies surgical find/replace patches to an existing personal skill.
///
/// Unlike [`super::crud::UpdateSkillTool`], which requires a full SKILL.md
/// rewrite, this tool applies one or more exact-string replacements, reducing
/// hallucination risk and token cost.
pub struct PatchSkillTool {
    bus: Arc<chelix_call_bus::CallBus>,
    usage_store: Option<SkillUsageStore>,
}

impl PatchSkillTool {
    pub fn new(bus: Arc<chelix_call_bus::CallBus>) -> Self {
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
impl AgentTool for PatchSkillTool {
    fn name(&self) -> &str {
        "patch_skill"
    }

    fn description(&self) -> &str {
        "Apply surgical find/replace patches to an existing personal skill's SKILL.md. \
         More efficient than update_skill when fixing a few lines — avoids regenerating \
         the entire body."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["name", "patches"],
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Skill name to patch"
                },
                "patches": {
                    "type": "array",
                    "description": "Ordered list of find/replace operations applied sequentially",
                    "minItems": 1,
                    "maxItems": MAX_PATCHES_PER_CALL,
                    "items": {
                        "type": "object",
                        "required": ["find", "replace"],
                        "properties": {
                            "find": {
                                "type": "string",
                                "description": "Exact string to find in the skill body"
                            },
                            "replace": {
                                "type": "string",
                                "description": "Replacement string"
                            }
                        }
                    }
                },
                "description": {
                    "type": "string",
                    "description": "Optional: update the frontmatter description"
                }
            }
        })
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
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::message("missing 'name'"))?;
        let patches = params
            .get("patches")
            .and_then(|v| v.as_array())
            .ok_or_else(|| Error::message("missing 'patches'"))?;
        let new_description = params.get("description").and_then(|v| v.as_str());

        if patches.is_empty() {
            return Err(Error::message("at least one patch is required").into());
        }
        if patches.len() > MAX_PATCHES_PER_CALL {
            return Err(Error::message(format!(
                "too many patches: maximum is {MAX_PATCHES_PER_CALL}"
            ))
            .into());
        }

        let mut replacements = Vec::with_capacity(patches.len());
        for (i, patch) in patches.iter().enumerate() {
            let find = patch
                .get("find")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::message(format!("patch[{i}]: missing 'find'")))?;
            let replace = patch
                .get("replace")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::message(format!("patch[{i}]: missing 'replace'")))?;
            replacements.push((find.to_string(), replace.to_string()));
        }
        let patched = self
            .bus
            .call(chelix_call_bus::PatchSkillFile {
                name: name.into(),
                actor: SkillActor::Agent { id: id.into() },
                patches: replacements.clone(),
                description: new_description.map(str::to_string),
            })
            .await
            .map_err(|error| super::helpers::missing_skill(name, error))?;
        let applied = replacements.len();
        let patched_body = patched.body;

        let hits = chelix_skills::safety::scan_skill_body(name, &patched_body);
        let warning = if !hits.is_empty() {
            tracing::warn!(
                skill = %name,
                patterns = ?hits,
                "patched skill body contains potential prompt-injection patterns"
            );
            Some(format!(
                "Warning: patched body matches injection patterns: {}",
                hits.join(", ")
            ))
        } else {
            None
        };

        if let Some(ref store) = self.usage_store {
            store.record_write(name).await;
        }
        #[cfg(feature = "metrics")]
        counter!(skills_metrics::MODIFICATIONS_TOTAL, labels::TOOL => "patch_skill".to_string())
            .increment(1);

        let mut response = json!({
            "patched": true,
            "patches_applied": applied,
        });
        if let Some(warn_msg) = warning
            && let Some(m) = response.as_object_mut()
        {
            m.insert("warning".into(), json!(warn_msg));
        }

        Ok(response)
    }
}
