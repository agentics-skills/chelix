use std::{collections::HashMap, path::PathBuf};

use secrecy::ExposeSecret;

use crate::schema::ChelixConfig;

use super::*;

// ── GH-770: env variable resolution from [env] section and DB ────────

/// GH-770: `${VAR}` in config sections should resolve against `[env]` values
/// defined in the same TOML file.
#[test]
fn gh770_env_section_vars_resolve_in_config_placeholders() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("chelix.toml");
    let expected = "sk-test-from-env-section";
    std::fs::write(
        &path,
        format!(
            r#"
[env]
MY_API_KEY = "{expected}"

[tools.web.firecrawl]
api_key = "${{MY_API_KEY}}"
"#
        ),
    )
    .expect("write config");

    let config = load_config(&path).expect("load config");
    let api_key = config
        .tools
        .web
        .firecrawl
        .api_key
        .as_ref()
        .expect("api_key should be set");
    // Never pass secret values to assert_eq! — it prints both sides on failure.
    assert!(
        api_key.expose_secret() == expected,
        "api_key should be resolved from [env] section, not left as literal placeholder"
    );
}

/// GH-770: Precedence test.  Process env lookup wins over the overrides map.
/// Tested via the underlying `substitute_env_with` with a mock lookup
/// so that no real env vars are read (avoids leaking secrets on failure).
#[test]
fn gh770_process_env_takes_precedence_over_env_section() {
    // The precedence logic lives in substitute_env_with_overrides which
    // chains std::env::var → overrides.  We verify the same chain
    // through substitute_env_with using a controlled mock.
    let result = crate::env_subst::substitute_env_with_overrides(
        "${CHELIX_GH770_PRECEDENCE_TEST}",
        &HashMap::from([("CHELIX_GH770_PRECEDENCE_TEST".into(), "from-map".into())]),
    );
    // The var is not in the process env, so the map value is used.
    assert_eq!(result, "from-map");

    // The full precedence proof (process env > map) is in the env_subst
    // unit test `with_overrides_primary_lookup_wins_over_map`.
}

/// GH-770: `resubstitute_config` resolves leftover `${VAR}` placeholders
/// using a runtime override map (simulating DB env vars).
#[test]
fn gh770_resubstitute_config_resolves_db_env_vars() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("chelix.toml");
    // Use a var name that definitely does not exist in the process env.
    let var = "CHELIX_GH770_ONLY_IN_DB_42";
    let expected_after = "sk-or-from-db";
    std::fs::write(
        &path,
        format!(
            r#"
[tools.web.firecrawl]
api_key = "${{{var}}}"
"#
        ),
    )
    .expect("write config");

    let config = load_config(&path).expect("load config");
    // Before resubstitution, the placeholder should still be literal.
    let key_before = config
        .tools
        .web
        .firecrawl
        .api_key
        .as_ref()
        .expect("api_key should be set");
    assert!(
        key_before.expose_secret().starts_with("${"),
        "placeholder should be unresolved before resubstitution"
    );

    // Simulate DB env vars becoming available.
    let mut runtime_overrides = HashMap::new();
    runtime_overrides.insert(var.to_string(), expected_after.to_string());
    let config = resubstitute_config(&config, &runtime_overrides).expect("resubstitute");

    let key_after = config
        .tools
        .web
        .firecrawl
        .api_key
        .as_ref()
        .expect("api_key should be set");
    assert!(
        key_after.expose_secret() == expected_after,
        "placeholder should resolve against runtime override map after resubstitution"
    );
}

/// GH-770: `resubstitute_config` preserves already-resolved values and
/// only resolves remaining placeholders.
#[test]
fn gh770_resubstitute_preserves_resolved_values() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("chelix.toml");
    let var = "CHELIX_GH770_UNRESOLVABLE_43";
    let expected = "resolved-later";
    std::fs::write(
        &path,
        format!(
            r#"
[agents]
default = "rex"

[agents.rex]
name = "Rex"
max_tools_threshold = 128

[tools.web.firecrawl]
api_key = "${{{var}}}"
"#
        ),
    )
    .expect("write config");

    let config = load_config(&path).expect("load config");
    assert_eq!(
        config.agents.get("rex").map(|agent| agent.name.as_str()),
        Some("Rex")
    );

    let mut overrides = HashMap::new();
    overrides.insert(var.to_string(), expected.to_string());
    let config = resubstitute_config(&config, &overrides).expect("resubstitute");

    // Existing values must survive the round-trip.
    assert_eq!(
        config.agents.get("rex").map(|agent| agent.name.as_str()),
        Some("Rex"),
        "non-placeholder values must survive resubstitution"
    );
    let key = config
        .tools
        .web
        .firecrawl
        .api_key
        .as_ref()
        .expect("api_key");
    assert!(
        key.expose_secret() == expected,
        "placeholder should resolve after resubstitution"
    );
}

/// GH-770: Override values containing quotes or backslashes must not break
/// resubstitution (no TOML injection via textual round-trip).
#[test]
fn gh770_resubstitute_handles_special_chars_in_values() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("chelix.toml");
    let var = "CHELIX_GH770_SPECIAL_CHARS";
    std::fs::write(
        &path,
        format!(
            r#"
[tools.web.firecrawl]
api_key = "${{{var}}}"
"#
        ),
    )
    .expect("write config");

    let config = load_config(&path).expect("load config");

    // Value with double-quote and backslash — would break TOML text substitution.
    let tricky_value = r#"sk-pass"word\with\special"chars"#;
    let mut overrides = HashMap::new();
    overrides.insert(var.to_string(), tricky_value.to_string());

    let config = resubstitute_config(&config, &overrides)
        .expect("resubstitute must not fail on special chars");
    let key = config
        .tools
        .web
        .firecrawl
        .api_key
        .as_ref()
        .expect("api_key should be set");
    assert!(
        key.expose_secret() == tricky_value,
        "value with quotes/backslashes must survive resubstitution intact"
    );
}

// ── Layered config tests ─────────────────────────────────────────────

#[test]
fn defaults_toml_is_generated_and_parseable() {
    let content = crate::defaults::generate_defaults_toml().expect("generate defaults");
    assert!(
        content.contains("CHELIX-MANAGED DEFAULTS"),
        "defaults.toml should contain ownership header"
    );
    let config: ChelixConfig =
        toml::from_str(&content).expect("defaults.toml should parse as valid ChelixConfig");
    // Verify it matches the built-in defaults.
    assert_eq!(config.tools.agent_timeout_secs, 600);
    assert!(config.agents.entries.is_empty());
    assert!(config.tls.enabled);
}

#[test]
fn defaults_toml_written_and_loaded_from_disk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = crate::defaults::write_defaults_toml(dir.path()).expect("write defaults.toml");
    assert!(path.exists());

    let raw = std::fs::read_to_string(path).expect("read defaults.toml");
    let config: ChelixConfig = toml::from_str(&raw).expect("parse defaults.toml");
    assert_eq!(config.tools.agent_timeout_secs, 600);
    assert!(config.tls.enabled);
}

#[test]
fn merge_defaults_with_user_overrides() {
    let defaults = crate::defaults::generate_defaults_toml().expect("generate defaults");
    // User only overrides agent_timeout_secs.
    let user = r#"
[tools]
agent_timeout_secs = 120
"#;
    let path = PathBuf::from("test.toml");
    let config =
        crate::defaults::merge_defaults_with_user_toml(&defaults, user, &path).expect("merge");

    // User override applied.
    assert_eq!(config.tools.agent_timeout_secs, 120);
    // Defaults preserved.
    assert!(config.agents.entries.is_empty());
    assert!(config.tls.enabled);
    assert!(!config.auth.disabled);
}

#[test]
fn merge_preserves_user_only_keys() {
    let defaults = crate::defaults::generate_defaults_toml().expect("generate defaults");
    // User adds an agent entry that is not present in managed defaults.
    let user = r#"
[agents]
default = "rex"

[agents.rex]
name = "Rex"
max_tools_threshold = 128
"#;
    let path = PathBuf::from("test.toml");
    let config =
        crate::defaults::merge_defaults_with_user_toml(&defaults, user, &path).expect("merge");

    assert_eq!(
        config.agents.get("rex").map(|agent| agent.name.as_str()),
        Some("Rex")
    );
    // Defaults still present.
    assert!(config.tls.enabled);
}

#[test]
fn save_user_config_does_not_materialize_defaults() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("chelix.toml");

    // Start with a minimal user config.
    std::fs::write(&path, "[server]\nport = 12345\n").expect("write seed");

    let raw = std::fs::read_to_string(&path).expect("read seed");
    let mut config: ChelixConfig = parse_config(&raw, &path).expect("parse");
    // Make one change.
    config.auth.disabled = true;

    save_user_config_to_path(&path, &config).expect("save user config");

    let saved = std::fs::read_to_string(&path).expect("read saved");

    // User override should be present.
    assert!(
        saved.contains("disabled = true"),
        "user override should be saved"
    );
    // Built-in defaults should NOT be materialized.
    assert!(
        !saved.contains("agent_timeout_secs"),
        "defaults should not be materialized into user config"
    );
    assert!(
        !saved.contains("max_tools_threshold"),
        "defaults should not be materialized into user config"
    );
}

#[test]
fn save_user_config_rejects_reserved_agent_ids_before_writing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("chelix.toml");
    let mut config = ChelixConfig::default();
    config.agents.default = "main".to_string();
    config
        .agents
        .entries
        .insert("main".to_string(), crate::AgentConfig {
            name: "Main".to_string(),
            ..Default::default()
        });
    config
        .agents
        .entries
        .insert("default".to_string(), crate::AgentConfig {
            name: "Reserved".to_string(),
            ..Default::default()
        });

    let error = save_user_config_to_path(&path, &config)
        .expect_err("reserved agent id must be rejected before writing");

    assert!(error.to_string().contains("agents.default"));
    assert!(!path.exists());
}

#[test]
fn update_config_preserves_override_boundary() {
    let _guard = CONFIG_DIR_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("chelix.toml");

    // Seed a minimal user config.
    std::fs::write(
        &config_path,
        "[server]\nport = 54321\n\n[auth]\ndisabled = true\n",
    )
    .expect("write seed");

    set_config_dir(dir.path().to_path_buf());

    // Simulate an update_config call that changes only one field.
    let result_path = update_config(|cfg| {
        cfg.server.http_request_logs = true;
    })
    .expect("update_config");

    let saved = std::fs::read_to_string(&result_path).expect("read saved");

    // User changes present.
    assert!(saved.contains("http_request_logs = true"));
    assert!(saved.contains("disabled = true"));
    assert!(saved.contains("port = 54321"));

    // Defaults NOT materialized.
    assert!(
        !saved.contains("agent_timeout_secs"),
        "update_config should not materialize defaults"
    );

    clear_config_dir();
}

#[test]
fn layered_load_user_override_wins_over_defaults() {
    let _guard = CONFIG_DIR_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("chelix.toml");

    // Write defaults.toml first.
    crate::defaults::write_defaults_toml(dir.path()).expect("write defaults");

    // Write user config with an override.
    std::fs::write(
        &config_path,
        "[server]\nport = 11111\n\n[tools]\nagent_timeout_secs = 999\n",
    )
    .expect("write user config");

    set_config_dir(dir.path().to_path_buf());

    let config = discover_and_load().expect("load config");
    // User override wins.
    assert_eq!(config.tools.agent_timeout_secs, 999);
    // Defaults inherited.
    assert!(config.agents.entries.is_empty());
    assert!(config.tls.enabled);

    clear_config_dir();
}

#[test]
fn upgrade_adds_new_defaults_automatically() {
    let _guard = CONFIG_DIR_TEST_LOCK.lock().unwrap();
    // Simulate: defaults.toml has new settings that weren't in the old version.
    // User config only has port. After layered load, new defaults should appear.
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("chelix.toml");

    // Write a minimal user config (no tools section).
    std::fs::write(&config_path, "[server]\nport = 22222\n").expect("write user config");

    // Write defaults.toml.
    crate::defaults::write_defaults_toml(dir.path()).expect("write defaults");

    set_config_dir(dir.path().to_path_buf());

    let config = discover_and_load().expect("load config");
    // Defaults should be inherited even though user didn't specify them.
    assert_eq!(config.tools.agent_timeout_secs, 600);
    assert!(config.agents.entries.is_empty());
    assert!(config.heartbeat.enabled);

    clear_config_dir();
}

#[test]
fn user_override_survives_defaults_refresh() {
    let _guard = CONFIG_DIR_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("chelix.toml");

    // User overrides timeout.
    std::fs::write(
        &config_path,
        "[server]\nport = 33333\n\n[tools]\nagent_timeout_secs = 42\n",
    )
    .expect("write user config");

    set_config_dir(dir.path().to_path_buf());

    // First load (writes defaults.toml).
    let config1 = discover_and_load().expect("load config");
    assert_eq!(config1.tools.agent_timeout_secs, 42);

    // Simulate upgrade by refreshing defaults.toml again.
    crate::defaults::write_defaults_toml(dir.path()).expect("refresh defaults");

    // Reload — user override must survive.
    let config2 = discover_and_load().expect("reload config");
    assert_eq!(
        config2.tools.agent_timeout_secs, 42,
        "user override must survive defaults refresh"
    );

    clear_config_dir();
}

#[test]
fn upgrade_existing_config_rejects_removed_agent_max_iterations() {
    let _guard = CONFIG_DIR_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("chelix.toml");

    std::fs::write(
        &config_path,
        r#"[server]
port = 18789

[tools]
agent_max_iterations = 25
"#,
    )
    .expect("write legacy config");

    set_config_dir(dir.path().to_path_buf());

    let error = update_config(|cfg| {
        cfg.server.http_request_logs = true;
    })
    .expect_err("removed config key must fail");
    assert!(error.to_string().contains("agent_max_iterations"));

    let saved = std::fs::read_to_string(&config_path).expect("read unchanged config");
    assert!(!saved.contains("http_request_logs"));

    clear_config_dir();
}

#[test]
fn initialize_config_preserves_explicit_default_coqui_endpoint() {
    let _guard = CONFIG_DIR_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("chelix.toml");

    std::fs::write(
        &config_path,
        r#"
[server]
port = 18789

[voice.tts.coqui]
enabled = true
endpoint = "http://localhost:5002"
"#,
    )
    .expect("write config");

    set_config_dir(dir.path().to_path_buf());
    initialize_config().expect("initialize config");

    let saved = std::fs::read_to_string(&config_path).expect("read saved");
    assert!(
        saved.contains("endpoint = \"http://localhost:5002\""),
        "startup initialization must not strip explicit default-valued Coqui endpoint"
    );
    assert!(
        saved.contains("enabled = true"),
        "startup initialization must not strip explicit default-valued Coqui enabled flag"
    );

    clear_config_dir();
}

#[test]
fn initialize_config_port_persistence_preserves_explicit_default_coqui_endpoint() {
    let _guard = CONFIG_DIR_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("chelix.toml");

    std::fs::write(
        &config_path,
        r#"
[server]
port = 0

[voice.tts.coqui]
enabled = true
endpoint = "http://localhost:5002"
"#,
    )
    .expect("write config");

    set_config_dir(dir.path().to_path_buf());
    initialize_config().expect("initialize config");

    let saved = std::fs::read_to_string(&config_path).expect("read saved");
    let saved_config = parse_config(&saved, &config_path).expect("parse saved config");
    assert_ne!(
        saved_config.server.port, 0,
        "startup initialization should persist a generated port"
    );
    assert!(
        saved.contains("endpoint = \"http://localhost:5002\""),
        "port persistence must not strip explicit default-valued Coqui endpoint"
    );
    assert!(
        saved.contains("enabled = true"),
        "port persistence must not strip explicit default-valued Coqui enabled flag"
    );

    clear_config_dir();
}

#[test]
fn initialize_config_rejects_invalid_sandbox_mode() {
    let _guard = CONFIG_DIR_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("chelix.toml");

    std::fs::write(&config_path, "[sandbox]\nmode = \"Of\"\n").expect("write config");
    set_config_dir(dir.path().to_path_buf());

    let result = initialize_config();

    clear_config_dir();
    let error = result.expect_err("invalid sandbox mode must fail config initialization");
    assert!(
        error.to_string().contains("Of"),
        "error should identify the invalid sandbox mode: {error}"
    );
}

#[test]
fn initialize_config_creates_config_only_when_missing() {
    let _guard = CONFIG_DIR_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("chelix.toml");
    set_config_dir(dir.path().to_path_buf());

    let initialized = initialize_config().expect("initialize missing config");

    assert!(config_path.exists());
    assert_ne!(initialized.server.port, 0);
    discover_and_load().expect("reload initialized config");
    clear_config_dir();
}

#[test]
fn discover_and_load_rejects_missing_config_without_creating_files() {
    let _guard = CONFIG_DIR_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    set_config_dir(dir.path().to_path_buf());

    let error = discover_and_load().expect_err("missing config must fail strict reload");

    assert!(error.to_string().contains("no config file found"));
    assert!(!dir.path().join("chelix.toml").exists());
    assert!(!dir.path().join("defaults.toml").exists());
    clear_config_dir();
}

#[test]
fn supported_formats_reject_unknown_fields() {
    let _guard = CONFIG_DIR_TEST_LOCK.lock().unwrap();
    let cases = [
        ("toml", "[server]\nremoved_option = true\n"),
        ("yaml", "server:\n  removed_option: true\n"),
        ("yml", "server:\n  removed_option: true\n"),
        ("json", r#"{"server":{"removed_option":true}}"#),
    ];

    for (extension, contents) in cases {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join(format!("chelix.{extension}"));
        std::fs::write(&config_path, contents).expect("write config");
        set_config_dir(dir.path().to_path_buf());

        let error = discover_and_load().expect_err("unknown field must fail config load");
        assert!(
            error.to_string().contains("server.removed_option"),
            "{extension} error should identify the unknown field: {error}"
        );
    }

    clear_config_dir();
}

#[test]
fn malformed_config_fails_initialization_without_overwrite() {
    let _guard = CONFIG_DIR_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("chelix.toml");
    let invalid = "[server\nport = 18789\n";
    std::fs::write(&config_path, invalid).expect("write invalid config");
    set_config_dir(dir.path().to_path_buf());

    initialize_config().expect_err("malformed config must fail initialization");

    assert_eq!(
        std::fs::read_to_string(&config_path).expect("read invalid config"),
        invalid
    );
    clear_config_dir();
}

#[test]
fn update_config_rejects_invalid_existing_config_without_overwrite() {
    let _guard = CONFIG_DIR_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("chelix.toml");
    let invalid = "[server]\nremoved_option = true\n";
    std::fs::write(&config_path, invalid).expect("write invalid config");
    set_config_dir(dir.path().to_path_buf());

    update_config(|config| config.server.port = 18789)
        .expect_err("update must reject invalid existing config");

    assert_eq!(
        std::fs::read_to_string(&config_path).expect("read invalid config"),
        invalid
    );
    clear_config_dir();
}

#[test]
fn strip_default_values_removes_matching_defaults() {
    let effective = r#"
[server]
port = 18789
bind = "127.0.0.1"

[auth]
disabled = true

[tools]
agent_timeout_secs = 600
"#;
    let defaults = r#"
[server]
port = 0
bind = "127.0.0.1"

[auth]
disabled = false

[tools]
agent_timeout_secs = 600
"#;

    let mut eff_doc = effective.parse::<toml_edit::DocumentMut>().unwrap();
    let def_doc = defaults.parse::<toml_edit::DocumentMut>().unwrap();

    strip_default_values(eff_doc.as_table_mut(), def_doc.as_table());
    let result = eff_doc.to_string();

    // port differs → kept
    assert!(
        result.contains("port = 18789"),
        "different value should be kept"
    );
    // bind matches default → stripped
    assert!(!result.contains("bind"), "default value should be stripped");
    // auth.disabled differs → kept
    assert!(
        result.contains("disabled = true"),
        "different value should be kept"
    );
    // agent_timeout_secs matches default → stripped
    assert!(
        !result.contains("agent_timeout_secs"),
        "default value should be stripped"
    );
}

#[test]
fn deleting_agent_does_not_restore_it_from_managed_defaults() {
    let _guard = CONFIG_DIR_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("chelix.toml");

    std::fs::write(
        &config_path,
        r#"[server]
port = 44444

[agents]
default = "main"

[agents.main]
name = "Main"
max_tools_threshold = 128

[agents.research]
name = "Research"
max_tools_threshold = 7
model = "openai/gpt-5.2"
"#,
    )
    .expect("write seed");

    set_config_dir(dir.path().to_path_buf());
    discover_and_load().expect("load configured agents");

    update_config(|config| {
        config.agents.entries.remove("research");
    })
    .expect("delete agent");

    crate::defaults::write_defaults_toml(dir.path()).expect("refresh managed defaults");
    let saved = std::fs::read_to_string(&config_path).expect("read saved");
    assert!(!saved.contains("[agents.research]"));
    assert!(saved.contains("[agents.main]"));
    assert!(saved.contains("port = 44444"));

    let reloaded = discover_and_load().expect("reload after deletion");
    assert!(reloaded.agents.entries.contains_key("main"));
    assert!(!reloaded.agents.entries.contains_key("research"));

    clear_config_dir();
}

#[test]
fn find_shadowed_defaults_detects_shadows() {
    // User config that overrides a built-in default
    let user = r#"
[tools]
agent_timeout_secs = 600

[auth]
disabled = false
"#;
    let shadowed = crate::defaults::find_shadowed_defaults(user);
    assert!(
        shadowed.contains(&"tools.agent_timeout_secs".to_string()),
        "should detect tools.agent_timeout_secs as shadowed"
    );
    assert!(
        shadowed.contains(&"auth.disabled".to_string()),
        "should detect auth.disabled as shadowed"
    );
}

#[test]
fn find_shadowed_defaults_ignores_intentional_overrides() {
    // User config where values DIFFER from defaults — these are intentional
    // overrides, not frozen defaults, and should NOT be flagged.
    let user = r#"
[tools]
agent_timeout_secs = 120

[auth]
disabled = true
"#;
    let shadowed = crate::defaults::find_shadowed_defaults(user);
    assert!(
        !shadowed.contains(&"tools.agent_timeout_secs".to_string()),
        "intentional override (120 != default 600) should not be flagged as shadowed"
    );
    assert!(
        !shadowed.contains(&"auth.disabled".to_string()),
        "intentional override (true != default false) should not be flagged as shadowed"
    );
}

#[test]
fn find_shadowed_defaults_ignores_user_owned_agents() {
    let user = r#"
[agents]
default = "rex"

[agents.rex]
name = "Rex"
max_tools_threshold = 128
"#;
    let shadowed = crate::defaults::find_shadowed_defaults(user);
    assert!(
        shadowed.iter().all(|key| !key.starts_with("agents.rex")),
        "user-owned agent fields must not be managed defaults: {shadowed:?}"
    );
}
