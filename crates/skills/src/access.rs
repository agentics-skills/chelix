use std::path::Path;

use {chelix_config::schema::AgentSkillPolicy, serde_yaml::Value};

use crate::{
    error::{Error, Result},
    parse::{metadata_frontmatter_value, parse_metadata, split_frontmatter_raw},
    types::SkillMetadata,
};

/// Decide skill visibility using exact agent ids, then names and categories.
#[must_use]
pub fn visible_to_agent(agent_id: &str, skill: &SkillMetadata, policy: &AgentSkillPolicy) -> bool {
    if skill.deny.iter().any(|id| id == agent_id) {
        return false;
    }
    if !skill.allow.is_empty() {
        return skill.allow.iter().any(|id| id == agent_id);
    }
    let matches = |entry: &String| {
        entry == &skill.name
            || skill
                .category
                .as_deref()
                .is_some_and(|category| entry == category)
    };
    (policy.allow.is_empty() || policy.allow.iter().any(matches))
        && !policy.deny.iter().any(matches)
}

/// Publish a skill with its persisted access lists and the supplied body bytes.
pub async fn publish_markdown(path: &Path, content: &str, on_create: Option<&str>) -> Result<()> {
    let skill_dir = path
        .parent()
        .ok_or_else(|| Error::Validation("SKILL.md must have a parent directory".into()))?;
    let (allow, deny) = match tokio::fs::read_to_string(path).await {
        Ok(current) => {
            let metadata = parse_metadata(&current, skill_dir)?;
            (metadata.allow, metadata.deny)
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (
            on_create.into_iter().map(String::from).collect(),
            Vec::new(),
        ),
        Err(error) => return Err(error.into()),
    };
    let (frontmatter, body) = split_frontmatter_raw(content)?;
    let mut value = metadata_frontmatter_value(frontmatter)?;
    let mapping = value
        .as_mapping_mut()
        .ok_or_else(|| Error::Parse("invalid SKILL.md frontmatter: expected a mapping".into()))?;
    mapping.insert(Value::String("allow".into()), serde_yaml::to_value(allow)?);
    mapping.insert(Value::String("deny".into()), serde_yaml::to_value(deny)?);
    let frontmatter = serde_yaml::to_string(&value)?;
    let published = format!("---\n{frontmatter}---{body}");
    tokio::fs::write(path, published).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visibility_prioritizes_skill_lists_then_agent_policy() {
        let mut skill = SkillMetadata {
            name: "demo".into(),
            category: Some("research".into()),
            ..Default::default()
        };
        let policy = AgentSkillPolicy {
            allow: vec!["other".into()],
            deny: vec!["demo".into()],
        };
        skill.allow = vec!["agent1".into()];
        assert!(visible_to_agent("agent1", &skill, &policy));
        assert!(!visible_to_agent(
            "agent2",
            &skill,
            &AgentSkillPolicy::default()
        ));
        assert!(!visible_to_agent(" agent1", &skill, &policy));
        skill.deny = vec!["agent1".into()];
        assert!(!visible_to_agent("agent1", &skill, &policy));
        let named_policy = AgentSkillPolicy {
            allow: vec!["demo".into()],
            deny: Vec::new(),
        };
        assert!(!visible_to_agent("agent1", &skill, &named_policy));
        skill.allow.clear();
        assert!(!visible_to_agent(
            "agent1",
            &skill,
            &AgentSkillPolicy::default()
        ));
        skill.deny = vec!["agent2".into()];
        let whitelist = AgentSkillPolicy {
            allow: vec!["other".into()],
            deny: Vec::new(),
        };
        assert!(!visible_to_agent("agent1", &skill, &whitelist));
        let category_policy = AgentSkillPolicy {
            allow: vec!["research".into()],
            deny: Vec::new(),
        };
        assert!(visible_to_agent("agent1", &skill, &category_policy));
        assert!(visible_to_agent(
            "agent1",
            &skill,
            &AgentSkillPolicy::default()
        ));
        assert!(!visible_to_agent(
            "agent2",
            &skill,
            &AgentSkillPolicy::default()
        ));
        let denied_category = AgentSkillPolicy {
            allow: Vec::new(),
            deny: vec!["research".into()],
        };
        assert!(!visible_to_agent("agent1", &skill, &denied_category));
    }

    #[tokio::test]
    async fn publish_preserves_string_metadata_scalars() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        for name in ["123", "0x10"] {
            let path = tmp.path().join(format!("{name}.md"));
            let content = format!(
                "---\nname: {name}\ndescription: {name}\nallowed-tools: [read_file]\norigin:\n  version: {name}\ncustom: unchanged\n---\nbody"
            );
            publish_markdown(&path, &content, None).await?;
            let published = tokio::fs::read_to_string(&path).await?;
            let metadata = parse_metadata(&published, tmp.path())?;
            assert_eq!(metadata.name, name);
            assert_eq!(metadata.description, name);
            assert_eq!(metadata.allowed_tools, ["read_file"]);
            assert_eq!(
                metadata.origin.and_then(|origin| origin.version).as_deref(),
                Some(name)
            );
            assert!(published.contains("custom: unchanged"));
            publish_markdown(&path, &published, Some("agent1")).await?;
            assert_eq!(
                parse_metadata(&tokio::fs::read_to_string(&path).await?, tmp.path())?.name,
                name
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn publish_preserves_access_and_body_bytes() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("SKILL.md");
        let content = "---\nname: demo\ndescription: test\n---\r\n\r\n body  \r\n";
        publish_markdown(&path, content, Some("agent1")).await?;
        let created = tokio::fs::read_to_string(&path).await?;
        let meta = parse_metadata(&created, tmp.path())?;
        assert_eq!(meta.allow, ["agent1"]);
        assert!(meta.deny.is_empty());
        assert!(created.contains("deny: []"));
        assert_eq!(split_frontmatter_raw(&created)?.1, "\r\n\r\n body  \r\n");
        publish_markdown(&path, content, Some("agent2")).await?;
        let updated = tokio::fs::read_to_string(&path).await?;
        assert_eq!(parse_metadata(&updated, tmp.path())?.allow, ["agent1"]);
        tokio::fs::write(&path, content).await?;
        publish_markdown(&path, content, Some("agent2")).await?;
        let updated = tokio::fs::read_to_string(&path).await?;
        assert!(updated.contains("allow: []"));
        assert!(updated.contains("deny: []"));
        tokio::fs::remove_file(&path).await?;
        publish_markdown(&path, content, None).await?;
        let created = tokio::fs::read_to_string(&path).await?;
        assert!(created.contains("allow: []"));
        assert!(created.contains("deny: []"));
        Ok(())
    }
}
