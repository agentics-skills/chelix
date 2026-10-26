use std::{collections::HashMap, path::Path};

use secrecy::ExposeSecret;

use super::{parse_config_source, parse_raw_config, resubstitute_config};

fn path() -> &'static Path {
    Path::new("chelix.toml")
}

fn firecrawl_document(env_assignment: &str, placeholder: &str) -> String {
    format!(
        r#"
{env_assignment}
[tools.web.firecrawl]
api_key = "{placeholder}"
"#
    )
}

#[test]
fn parse_config_source_resolves_env_section_placeholders() {
    let expected = "sk-test-from-env-section";
    let raw = firecrawl_document(
        &format!("[env]\nMY_API_KEY = \"{expected}\"\n"),
        "${MY_API_KEY}",
    );

    let config = parse_config_source(&raw, path()).expect("parse substituted config");
    let api_key = config
        .tools
        .web
        .firecrawl
        .api_key
        .as_ref()
        .expect("api_key");
    assert!(
        api_key.expose_secret() == expected,
        "api_key should be resolved from [env]"
    );
}

#[test]
fn parse_raw_config_keeps_placeholders_and_reads_env() {
    let raw = firecrawl_document("[env]\nMY_API_KEY = \"from-env\"\n", "${MY_API_KEY}");

    let config = parse_raw_config(&raw, path()).expect("parse raw config");
    assert_eq!(
        config.env.get("MY_API_KEY").map(String::as_str),
        Some("from-env")
    );
    let api_key = config
        .tools
        .web
        .firecrawl
        .api_key
        .as_ref()
        .expect("api_key");
    assert!(
        api_key.expose_secret() == "${MY_API_KEY}",
        "raw parse must not substitute placeholders"
    );
}

#[test]
fn parse_rejects_unknown_fields() {
    let raw = "[server]\nremoved_option = true\n";
    let parsers: [fn(&str, &Path) -> crate::Result<crate::ChelixConfig>; 2] =
        [parse_raw_config, parse_config_source];
    for parse in parsers {
        let error = parse(raw, path()).expect_err("unknown field must fail");
        let message = error.to_string();
        assert!(message.contains("server.removed_option"), "{message}");
        assert!(message.contains("chelix.toml"), "{message}");
    }
}

#[test]
fn resubstitute_config_resolves_runtime_overrides() {
    let var = "CHELIX_GH770_ONLY_IN_DB_42";
    let expected = "sk-or-from-db";
    let raw = firecrawl_document("", &format!("${{{var}}}"));
    let config = parse_config_source(&raw, path()).expect("parse unresolved placeholder");
    let key_before = config
        .tools
        .web
        .firecrawl
        .api_key
        .as_ref()
        .expect("api_key");
    assert!(key_before.expose_secret().starts_with("${"));

    let config = resubstitute_config(
        &config,
        &HashMap::from([(var.to_string(), expected.to_string())]),
    )
    .expect("resubstitute");
    let key_after = config
        .tools
        .web
        .firecrawl
        .api_key
        .as_ref()
        .expect("api_key");
    assert!(key_after.expose_secret() == expected);
}

#[test]
fn resubstitute_config_preserves_existing_values() {
    let var = "CHELIX_GH770_UNRESOLVABLE_43";
    let expected = "resolved-later";
    let raw = format!(
        r#"
[agents]
default = "rex"

[agents.rex]
name = "Rex"
model = "test::model"
reasoning_effort = "off"
max_tools_threshold = 128
compaction_reminder = true
prepend_sender_badge = true

[tools.web.firecrawl]
api_key = "${{{var}}}"
"#
    );
    let config = parse_config_source(&raw, path()).expect("parse config");
    assert_eq!(
        config.agents.get("rex").map(|agent| agent.name.as_str()),
        Some("Rex")
    );

    let config = resubstitute_config(
        &config,
        &HashMap::from([(var.to_string(), expected.to_string())]),
    )
    .expect("resubstitute");
    assert_eq!(
        config.agents.get("rex").map(|agent| agent.name.as_str()),
        Some("Rex")
    );
    let key = config
        .tools
        .web
        .firecrawl
        .api_key
        .as_ref()
        .expect("api_key");
    assert!(key.expose_secret() == expected);
}

#[test]
fn resubstitute_config_keeps_quotes_and_backslashes() {
    let var = "CHELIX_GH770_SPECIAL_CHARS";
    let raw = firecrawl_document("", &format!("${{{var}}}"));
    let config = parse_config_source(&raw, path()).expect("parse config");
    let tricky_value = r#"sk-pass"word\with\special"chars"#;

    let config = resubstitute_config(
        &config,
        &HashMap::from([(var.to_string(), tricky_value.to_string())]),
    )
    .expect("resubstitute");
    let key = config
        .tools
        .web
        .firecrawl
        .api_key
        .as_ref()
        .expect("api_key");
    assert!(key.expose_secret() == tricky_value);
}
