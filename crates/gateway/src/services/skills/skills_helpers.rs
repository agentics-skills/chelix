use {
    chelix_call_bus::{
        ReadSkillFile, SetBundledSkillEnabled, SkillActor, SkillLocate, SkillMetadata,
    },
    serde_json::Value,
};

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
    bus: &chelix_call_bus::CallBus,
    source_type: &str,
    skill_name: &str,
) -> ServiceResult {
    let locate = if source_type == "personal" {
        SkillLocate::PersonalDir
    } else {
        SkillLocate::ProjectDir
    };
    let content = bus
        .call(ReadSkillFile {
            name: skill_name.into(),
            locate,
            actor: SkillActor::Process,
            file_path: None,
            max_bytes: None,
        })
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
        "path": content.metadata.path.to_string_lossy(),
    }))
}

/// Load a bundled skill through its owner.
pub(super) async fn skill_detail_bundled(
    bus: &chelix_call_bus::CallBus,
    skill_name: &str,
) -> ServiceResult {
    let content = bus
        .call(ReadSkillFile {
            name: skill_name.into(),
            max_bytes: None,
            actor: SkillActor::Process,
            locate: SkillLocate::Bundled,
            file_path: None,
        })
        .await
        .map_err(|error| error.to_string())?;
    let meta = &content.metadata;
    Ok(serde_json::json!({
        "name": meta.name, "description": meta.description, "category": meta.category,
        "license": meta.license, "compatibility": meta.compatibility, "origin": meta.origin,
        "file": skill_document_json(meta, &content.body), "enabled": content.enabled,
        "protected": true, "body": content.body, "body_html": markdown_to_html(&content.body), "source": "bundled"
    }))
}

/// Toggle a bundled skill through its owner.
pub(super) async fn toggle_bundled_skill(
    bus: &chelix_call_bus::CallBus,
    params: &Value,
    enabled: bool,
) -> ServiceResult {
    let skill_name = params
        .get("skill")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing 'skill' parameter".to_string())?;
    let result = bus
        .call(SetBundledSkillEnabled {
            name: skill_name.into(),
            enabled,
        })
        .await
        .map_err(|error| error.to_string())?;
    security_audit(
        "skills.skill.toggle",
        serde_json::json!({ "source": "bundled", "skill": skill_name, "category": result.category, "enabled": result.enabled }),
    );
    Ok(
        serde_json::json!({ "source": "bundled", "skill": skill_name, "category": result.category, "enabled": result.enabled }),
    )
}
