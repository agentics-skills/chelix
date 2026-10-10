use std::path::Path;

use serde::Deserialize;

use crate::error::{Error, Result};

use crate::types::SkillMetadata;

/// Validate a skill name: lowercase ASCII, hyphens, 1-64 chars.
pub fn validate_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == ':')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.starts_with(':')
        && !name.ends_with(':')
        && !name.contains("--")
        && !name.contains("::")
}

/// When `name` fails validation, use frontmatter `slug` as the internal name
/// and store the original `name` as `display_name`.
pub(crate) fn resolve_name_or_slug(meta: &mut SkillMetadata) -> Result<()> {
    if validate_name(&meta.name) {
        return Ok(());
    }

    match meta.slug.clone() {
        Some(ref slug) if validate_name(slug) => {
            tracing::debug!(
                name = %meta.name,
                slug = %slug,
                "skill name invalid, using slug as internal name"
            );
            meta.display_name = Some(std::mem::take(&mut meta.name));
            meta.name = slug.clone();
            Ok(())
        },
        Some(ref slug) => Err(Error::Validation(format!(
            "skill name '{}' is invalid and slug '{slug}' (from frontmatter) is also invalid: \
             must be 1-64 lowercase alphanumeric, hyphen, or colon chars \
             (e.g. 'my-skill' or 'ns:skill')",
            meta.name
        ))),
        None => Err(Error::Validation(format!(
            "skill name '{}' is invalid and no slug provided: \
             must be 1-64 lowercase alphanumeric, hyphen, or colon chars \
             (e.g. 'my-skill' or 'ns:skill'), \
             or provide a valid 'slug' field",
            meta.name
        ))),
    }
}

// ── _meta.json support (openclaw) ───────────────────────────────────────────

/// Metadata from an openclaw `_meta.json` file (sibling to SKILL.md).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SkillMetaJson {
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default, rename = "displayName")]
    pub display_name: Option<String>,
    #[serde(default)]
    pub latest: Option<SkillMetaVersion>,
}

/// Version info from `_meta.json`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SkillMetaVersion {
    #[serde(default)]
    pub version: Option<String>,
}

/// Try to read and parse `_meta.json` from a skill directory.
/// Returns `None` if the file doesn't exist or can't be parsed.
pub fn read_meta_json(skill_dir: &Path) -> Option<SkillMetaJson> {
    let path = skill_dir.join("_meta.json");
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

pub(crate) fn split_frontmatter_raw(content: &str) -> Result<(&str, &str)> {
    let after_open = content.trim_start().strip_prefix("---").ok_or_else(|| {
        Error::Parse("SKILL.md must start with YAML frontmatter delimited by ---".into())
    })?;
    let close_pos = after_open
        .find("\n---")
        .ok_or_else(|| Error::Parse("SKILL.md missing closing --- for frontmatter".into()))?;
    Ok((&after_open[..close_pos], &after_open[close_pos + 4..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    use rstest::rstest;

    #[rstest]
    #[case("my-skill", true)]
    #[case("a", true)]
    #[case("skill123", true)]
    #[case("plugin:skill", true)]
    #[case("pr-review-toolkit:code-reviewer", true)]
    #[case("", false)]
    #[case("-bad", false)]
    #[case("bad-", false)]
    #[case("Bad", false)]
    #[case("has space", false)]
    #[case("has--double", false)]
    #[case(":bad", false)]
    #[case("bad:", false)]
    #[case("bad::double", false)]
    fn test_validate_name(#[case] name: &str, #[case] expected: bool) {
        assert_eq!(validate_name(name), expected, "validate_name({name:?})");
    }

    #[test]
    fn test_validate_name_too_long() {
        assert!(!validate_name(&"a".repeat(65)));
    }
}
