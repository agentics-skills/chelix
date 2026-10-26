use serde_json::Value;

use chelix_skills::types::SkillMetadata;

// Pull in ServiceResult, ServiceError, security_audit, etc. from services module
use super::super::*;

/// Delete a personal or project skill directory to disable it.
pub(super) fn delete_discovered_skill(source_type: &str, params: &Value) -> ServiceResult {
    let skill_name = params
        .get("skill")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing 'skill' parameter".to_string())?;

    if is_protected_discovered_skill(skill_name) {
        return Err(ServiceError::forbidden(format!(
            "skill '{skill_name}' is protected and cannot be deleted from the UI"
        )));
    }

    if !chelix_skills::parse::validate_name(skill_name) {
        return Err(format!("invalid skill name '{skill_name}'").into());
    }

    let search_dir = if source_type == "personal" {
        chelix_config::data_dir().join("skills")
    } else {
        chelix_config::data_dir().join(".chelix/skills")
    };

    let skill_dir = search_dir.join(skill_name);
    if !skill_dir.exists() {
        return Err(format!("skill '{skill_name}' not found").into());
    }

    std::fs::remove_dir_all(&skill_dir)
        .map_err(|e| format!("failed to delete skill '{skill_name}': {e}"))?;

    security_audit(
        "skills.discovered.delete",
        serde_json::json!({
            "source": source_type,
            "skill": skill_name,
        }),
    );

    Ok(serde_json::json!({ "source": source_type, "skill": skill_name, "deleted": true }))
}

pub(super) fn skill_document_json(metadata: &SkillMetadata, body: &str) -> serde_json::Value {
    serde_json::json!({
        "name": metadata.name,
        "slug": metadata.slug,
        "display_name": metadata.display_name,
        "description": metadata.description,
        "homepage": metadata.homepage,
        "license": metadata.license,
        "compatibility": metadata.compatibility,
        "origin": metadata.origin,
        "allowed_agents": metadata.allowed_agents,
        "denied_agents": metadata.denied_agents,
        "body": body,
    })
}

/// Load skill detail for a personal or project skill by name.
pub(super) async fn skill_detail_discovered(
    files: &chelix_skills::skill_file::SkillFiles,
    source_type: &str,
    skill_name: &str,
) -> ServiceResult {
    // Build search paths for the requested source type.
    let search_dir = if source_type == "personal" {
        chelix_config::data_dir().join("skills")
    } else {
        chelix_config::data_dir().join(".chelix/skills")
    };

    let skill_dir = search_dir.join(skill_name);
    let content = chelix_skills::skill_file::read(files, &skill_dir.join("SKILL.md"), None)
        .await
        .map_err(|error| format!("failed to read SKILL.md for '{skill_name}': {error}"))?;

    Ok(serde_json::json!({
        "name": content.metadata.name,
        "description": content.metadata.description,
        "license": content.metadata.license,
        "compatibility": content.metadata.compatibility,
        "file": skill_document_json(&content.metadata, &content.body),
        "enabled": true,
        "protected": is_protected_discovered_skill(skill_name),
        "body": content.body,
        "body_html": markdown_to_html(&content.body),
        "source": source_type,
        "path": skill_dir.to_string_lossy(),
    }))
}

/// Load skill detail for a bundled skill by name.
#[cfg(feature = "bundled-skills")]
pub(super) async fn skill_detail_bundled(
    files: &Arc<chelix_skills::skill_file::SkillFiles>,
    configuration: &chelix_config_service::ConfigService,
    skill_name: &str,
) -> ServiceResult {
    let store = chelix_skills::bundled::BundledSkillStore::new(Arc::clone(files));
    let skills = store.discover().await;
    let meta = skills
        .iter()
        .find(|s| s.name == skill_name)
        .ok_or_else(|| format!("bundled skill '{skill_name}' not found"))?;

    let body = store
        .read_skill(skill_name)
        .await
        .ok_or_else(|| format!("bundled skill '{skill_name}' body not readable"))?;

    let config = crate::config_document::read_config(configuration)
        .await
        .map_err(ServiceError::message)?;
    let enabled = config
        .skills
        .is_bundled_skill_enabled(&meta.name, meta.category.as_deref());

    Ok(serde_json::json!({
        "name": meta.name,
        "description": meta.description,
        "category": meta.category,
        "license": meta.license,
        "compatibility": meta.compatibility,
        "origin": meta.origin,
        "file": skill_document_json(meta, &body),
        "enabled": enabled,
        "protected": true,
        "body": body,
        "body_html": markdown_to_html(&body),
        "source": "bundled",
    }))
}

/// Toggle a bundled skill by adding or removing its name from
/// `disabled_bundled_skills`.
pub(super) async fn toggle_bundled_skill(
    files: &Arc<chelix_skills::skill_file::SkillFiles>,
    configuration: &chelix_config_service::ConfigService,
    params: &Value,
    enabled: bool,
) -> ServiceResult {
    let skill_name = params
        .get("skill")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing 'skill' parameter".to_string())?;

    // Find the category for this bundled skill.
    #[cfg(feature = "bundled-skills")]
    {
        let store = chelix_skills::bundled::BundledSkillStore::new(Arc::clone(files));
        let skills = store.discover().await;
        let skill = skills
            .iter()
            .find(|s| s.name == skill_name)
            .ok_or_else(|| format!("bundled skill '{skill_name}' not found"))?;
        let category = skill.category.clone();

        let skill_name = skill_name.to_string();
        let persisted_skill = skill_name.clone();
        let persisted_category = category.clone();
        let effective_enabled = match configuration
            .update(move |cfg| {
                if enabled {
                    cfg.skills
                        .disabled_bundled_skills
                        .retain(|name| name != &persisted_skill);
                    if let Some(category) = &persisted_category {
                        cfg.skills
                            .disabled_bundled_categories
                            .retain(|name| name != category);
                    }
                } else if !cfg
                    .skills
                    .disabled_bundled_skills
                    .iter()
                    .any(|name| name == &persisted_skill)
                {
                    cfg.skills
                        .disabled_bundled_skills
                        .push(persisted_skill.clone());
                }
                Ok(cfg
                    .skills
                    .is_bundled_skill_enabled(&persisted_skill, persisted_category.as_deref()))
            })
            .await
        {
            Ok(enabled) => enabled,
            Err(e) => return Err(format!("failed to save config: {e}").into()),
        };

        security_audit(
            "skills.skill.toggle",
            serde_json::json!({
                "source": "bundled",
                "skill": skill_name,
                "category": category,
                "enabled": effective_enabled,
            }),
        );

        Ok(
            serde_json::json!({ "source": "bundled", "skill": skill_name, "category": category, "enabled": effective_enabled }),
        )
    }
    #[cfg(not(feature = "bundled-skills"))]
    {
        let _ = (skill_name, enabled, configuration);
        Err("bundled skills are not available in this build".into())
    }
}
