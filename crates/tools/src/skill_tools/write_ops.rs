//! Write and patch skill files: sidecar writes and surgical find/replace.

use std::{path::PathBuf, sync::Arc};

use {
    async_trait::async_trait,
    chelix_agents::tool_registry::AgentTool,
    chelix_skills::usage::SkillUsageStore,
    serde_json::{Value, json},
};

#[cfg(feature = "metrics")]
use chelix_metrics::{counter, labels, skills as skills_metrics};

use {
    super::{
        MAX_SIDECAR_FILES_PER_CALL,
        helpers::{audit_sidecar_file_write, validate_sidecar_files, write_sidecar_files},
    },
    crate::error::Error,
};

// ── WriteSkillFilesTool ─────────────────────────────────────

/// Tool that writes supplementary text files inside an existing personal skill.
pub struct WriteSkillFilesTool {
    data_dir: PathBuf,
    bus: Arc<chelix_call_bus::CallBus>,
}

impl WriteSkillFilesTool {
    pub fn new(data_dir: PathBuf, bus: Arc<chelix_call_bus::CallBus>) -> Self {
        Self { data_dir, bus }
    }

    fn skills_dir(&self) -> PathBuf {
        self.data_dir.join("skills")
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

    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::message("missing 'name'"))?;

        if !chelix_skills::parse::validate_name(name) {
            return Err(Error::message(format!(
                "invalid skill name '{name}': must be 1-64 lowercase alphanumeric/hyphen chars"
            ))
            .into());
        }

        let files = params
            .get("files")
            .and_then(|v| v.as_array())
            .ok_or_else(|| Error::message("missing 'files'"))?;
        let validated = validate_sidecar_files(files)?;

        let skill_dir = self.skills_dir().join(name);
        if !skill_dir.exists() {
            return Err(Error::message(format!(
                "skill '{name}' does not exist; use create_skill first"
            ))
            .into());
        }

        let skill_md_path = skill_dir.join("SKILL.md");
        self.bus
            .call(chelix_service_traits::ReadSkillFile {
                path: skill_md_path,
                max_bytes: None,
                mode: chelix_service_traits::SkillFileRead::Detect,
            })
            .await
            .map_err(|error| anyhow::anyhow!(error))?;
        write_sidecar_files(&skill_dir, &validated).await?;
        audit_sidecar_file_write(&self.data_dir, name, &validated);

        Ok(json!({
            "written": true,
            "path": skill_dir.display().to_string(),
            "files_written": validated.len(),
            "files": validated.iter().map(|file| file.relative_path.display().to_string()).collect::<Vec<_>>(),
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
    data_dir: PathBuf,
    bus: Arc<chelix_call_bus::CallBus>,
    usage_store: Option<SkillUsageStore>,
}

impl PatchSkillTool {
    pub fn new(data_dir: PathBuf, bus: Arc<chelix_call_bus::CallBus>) -> Self {
        Self {
            data_dir,
            bus,
            usage_store: None,
        }
    }

    #[must_use]
    pub fn with_usage_store(mut self, store: SkillUsageStore) -> Self {
        self.usage_store = Some(store);
        self
    }

    fn skills_dir(&self) -> PathBuf {
        self.data_dir.join("skills")
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

    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::message("missing 'name'"))?;
        let patches = params
            .get("patches")
            .and_then(|v| v.as_array())
            .ok_or_else(|| Error::message("missing 'patches'"))?;
        let new_description = params.get("description").and_then(|v| v.as_str());

        if !chelix_skills::parse::validate_name(name) {
            return Err(Error::message(format!(
                "invalid skill name '{name}': must be 1-64 lowercase alphanumeric/hyphen chars"
            ))
            .into());
        }
        if patches.is_empty() {
            return Err(Error::message("at least one patch is required").into());
        }
        if patches.len() > MAX_PATCHES_PER_CALL {
            return Err(Error::message(format!(
                "too many patches: maximum is {MAX_PATCHES_PER_CALL}"
            ))
            .into());
        }

        let skill_dir = self.skills_dir().join(name);
        if !skill_dir.exists() {
            return Err(Error::message(format!(
                "skill '{name}' does not exist; use create_skill first"
            ))
            .into());
        }

        let skill_md_path = skill_dir.join("SKILL.md");
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
            .call(chelix_service_traits::PatchSkillFile {
                path: skill_md_path,
                patches: replacements.clone(),
                description: new_description.map(str::to_string),
            })
            .await
            .map_err(|error| anyhow::anyhow!(error))?;
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
