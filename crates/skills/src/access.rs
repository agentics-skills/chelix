use chelix_config::schema::AgentSkillPolicy;

use crate::types::SkillMetadata;

/// Decide skill visibility using exact agent ids, then names and categories.
#[must_use]
pub fn visible_to_agent(agent_id: &str, skill: &SkillMetadata, policy: &AgentSkillPolicy) -> bool {
    if skill.denied_agents.iter().any(|id| id == agent_id) {
        return false;
    }
    if !skill.allowed_agents.is_empty() {
        return skill.allowed_agents.iter().any(|id| id == agent_id);
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

pub async fn discover_all(
    files: &crate::skill_file::SkillFiles,
    #[cfg(feature = "bundled-skills")] bundled: Option<&crate::bundled::BundledSkillStore>,
) -> crate::error::Result<Vec<SkillMetadata>> {
    let mut skills = Vec::new();
    for (path, source) in crate::discover::FsSkillDiscoverer::default_paths_for(files.root()?) {
        crate::discover::discover_flat(files, &path, &source, &mut skills).await?;
    }
    #[cfg(feature = "bundled-skills")]
    if let Some(store) = bundled {
        skills.extend(store.discover().await?);
    }
    Ok(skills)
}

pub async fn discover_skills(
    files: &crate::skill_file::SkillFiles,
    #[cfg(feature = "bundled-skills")] bundled: Option<&crate::bundled::BundledSkillStore>,
    config: &chelix_config::schema::SkillsConfig,
) -> crate::error::Result<Vec<SkillMetadata>> {
    let mut skills = discover_all(
        files,
        #[cfg(feature = "bundled-skills")]
        bundled,
    )
    .await?;
    if !config.enabled {
        return Ok(Vec::new());
    }
    skills.retain(|skill| {
        skill.source != Some(crate::types::SkillSource::Bundled)
            || config.is_bundled_skill_enabled(&skill.name, skill.category.as_deref())
    });
    Ok(skills)
}

pub fn filter_skills_for_agent(
    skills: Vec<SkillMetadata>,
    agent_id: &str,
    policy: &AgentSkillPolicy,
) -> Vec<SkillMetadata> {
    let mut seen = std::collections::HashSet::new();
    skills
        .into_iter()
        .filter(|skill| seen.insert(skill.name.clone()))
        .filter(|skill| visible_to_agent(agent_id, skill, policy))
        .collect()
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
        skill.allowed_agents = vec!["agent1".into()];
        assert!(visible_to_agent("agent1", &skill, &policy));
        assert!(!visible_to_agent(
            "agent2",
            &skill,
            &AgentSkillPolicy::default()
        ));
        assert!(!visible_to_agent(" agent1", &skill, &policy));
        skill.denied_agents = vec!["agent1".into()];
        assert!(!visible_to_agent("agent1", &skill, &policy));
        let named_policy = AgentSkillPolicy {
            allow: vec!["demo".into()],
            deny: Vec::new(),
        };
        assert!(!visible_to_agent("agent1", &skill, &named_policy));
        skill.allowed_agents.clear();
        assert!(!visible_to_agent(
            "agent1",
            &skill,
            &AgentSkillPolicy::default()
        ));
        skill.denied_agents = vec!["agent2".into()];
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
}
