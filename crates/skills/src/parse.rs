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
    #[case("ns:skill", true)]
    #[case("toolkit:code-reviewer", true)]
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
