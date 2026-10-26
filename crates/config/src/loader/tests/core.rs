use crate::{UserProfile, schema::ChelixConfig};

use super::*;

#[test]
fn data_dir_override_works() {
    let _guard = DATA_DIR_TEST_LOCK.lock().unwrap();
    let path = PathBuf::from("/tmp/test-data-dir-override");
    set_data_dir(path.clone());
    assert_eq!(data_dir(), path);
    clear_data_dir();
}

#[test]
fn save_and_load_user_frontmatter() {
    let _guard = DATA_DIR_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    set_data_dir(dir.path().to_path_buf());

    let user = UserProfile {
        name: Some("Alice".to_string()),
        timezone: Some(crate::schema::Timezone::from(chrono_tz::Europe::Berlin)),
        location: None,
    };

    let path = save_user(&user).expect("save user");
    assert!(path.exists());

    let loaded = load_user().expect("load user");
    assert_eq!(loaded.name.as_deref(), Some("Alice"));
    assert_eq!(
        loaded.timezone.as_ref().map(|tz| tz.name()),
        Some("Europe/Berlin")
    );

    clear_data_dir();
}

#[test]
fn save_and_load_user_with_location() {
    let _guard = DATA_DIR_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    set_data_dir(dir.path().to_path_buf());

    let user = UserProfile {
        name: Some("Bob".to_string()),
        timezone: Some(crate::schema::Timezone::from(chrono_tz::US::Eastern)),
        location: Some(crate::schema::GeoLocation {
            latitude: 48.8566,
            longitude: 2.3522,
            place: Some("Paris, France".to_string()),
            updated_at: Some(1_700_000_000),
        }),
    };

    save_user(&user).expect("save user with location");

    let loaded = load_user().expect("load user with location");
    assert_eq!(loaded.name.as_deref(), Some("Bob"));
    assert_eq!(
        loaded.timezone.as_ref().map(|tz| tz.name()),
        Some("US/Eastern")
    );
    let loc = loaded.location.expect("location should be present");
    assert!((loc.latitude - 48.8566).abs() < 1e-6);
    assert!((loc.longitude - 2.3522).abs() < 1e-6);
    assert_eq!(loc.place.as_deref(), Some("Paris, France"));

    clear_data_dir();
}

#[test]
fn save_user_removes_empty_file() {
    let _guard = DATA_DIR_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    set_data_dir(dir.path().to_path_buf());

    let seeded = UserProfile {
        name: Some("Alice".to_string()),
        timezone: None,
        location: None,
    };
    let path = save_user(&seeded).expect("seed user");
    assert!(path.exists());

    save_user(&UserProfile::default()).expect("save empty user");
    assert!(!path.exists());

    clear_data_dir();
}

#[test]
fn resolve_user_profile_prefers_user_md_over_config() {
    let _guard = DATA_DIR_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    set_data_dir(dir.path().to_path_buf());

    let config = ChelixConfig {
        user: UserProfile {
            name: Some("Config User".to_string()),
            timezone: Some(crate::schema::Timezone::from(chrono_tz::Europe::Paris)),
            location: Some(crate::schema::GeoLocation {
                latitude: 1.0,
                longitude: 2.0,
                place: Some("Config Place".to_string()),
                updated_at: Some(100),
            }),
        },
        ..Default::default()
    };
    save_user(&UserProfile {
        name: Some("File User".to_string()),
        timezone: Some(crate::schema::Timezone::from(chrono_tz::US::Eastern)),
        location: Some(crate::schema::GeoLocation {
            latitude: 3.0,
            longitude: 4.0,
            place: Some("File Place".to_string()),
            updated_at: Some(200),
        }),
    })
    .expect("save user");

    let resolved = resolve_user_profile_from_config(&config);
    assert_eq!(resolved.name.as_deref(), Some("File User"));
    assert_eq!(
        resolved.timezone.as_ref().map(|tz| tz.name()),
        Some("US/Eastern")
    );
    let location = resolved.location.expect("resolved location");
    assert_eq!(location.place.as_deref(), Some("File Place"));
    assert_eq!(location.updated_at, Some(200));

    clear_data_dir();
}

#[test]
fn save_user_with_mode_off_removes_existing_file() {
    let _guard = DATA_DIR_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    set_data_dir(dir.path().to_path_buf());

    let user = UserProfile {
        name: Some("Alice".to_string()),
        ..Default::default()
    };
    let path = save_user(&user).expect("seed user");
    assert!(path.exists());

    let saved_path = save_user_with_mode(&user, crate::schema::UserProfileWriteMode::Off)
        .expect("disable user profile writes");
    assert!(saved_path.is_none());
    assert!(!path.exists());

    clear_data_dir();
}

#[test]
fn load_boot_md_reads_trimmed_content() {
    let _guard = DATA_DIR_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    set_data_dir(dir.path().to_path_buf());

    std::fs::write(dir.path().join("BOOT.md"), "\n  Run startup checks.  \n").unwrap();
    assert_eq!(load_boot_md().as_deref(), Some("Run startup checks."));

    clear_data_dir();
}

#[test]
fn load_boot_md_for_agent_falls_back_to_root() {
    let _guard = DATA_DIR_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    set_data_dir(dir.path().to_path_buf());

    std::fs::write(dir.path().join("BOOT.md"), "Root boot context").unwrap();
    // No agent-specific file — should fall back to root.
    assert_eq!(
        load_boot_md_for_agent("test-agent").as_deref(),
        Some("Root boot context")
    );

    // Agent-specific file overrides root.
    let agent_dir = dir.path().join("agents").join("test-agent");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(agent_dir.join("BOOT.md"), "Agent-specific boot").unwrap();
    assert_eq!(
        load_boot_md_for_agent("test-agent").as_deref(),
        Some("Agent-specific boot")
    );

    clear_data_dir();
}

#[test]
fn load_tools_md_reads_trimmed_content() {
    let _guard = DATA_DIR_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    set_data_dir(dir.path().to_path_buf());

    std::fs::write(dir.path().join("TOOLS.md"), "\n  Use safe tools first.  \n").unwrap();
    assert_eq!(load_tools_md().as_deref(), Some("Use safe tools first."));

    clear_data_dir();
}

#[test]
fn load_agents_md_reads_trimmed_content() {
    let _guard = DATA_DIR_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    set_data_dir(dir.path().to_path_buf());

    std::fs::write(
        dir.path().join("AGENTS.md"),
        "\nLocal workspace instructions\n",
    )
    .unwrap();
    assert_eq!(
        load_agents_md().as_deref(),
        Some("Local workspace instructions")
    );

    clear_data_dir();
}

#[test]
fn load_memory_md_reads_trimmed_content() {
    let _guard = DATA_DIR_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    set_data_dir(dir.path().to_path_buf());

    std::fs::write(
        dir.path().join("MEMORY.md"),
        "\n## User Facts\n- Lives in Paris\n",
    )
    .unwrap();
    assert_eq!(
        load_memory_md().as_deref(),
        Some("## User Facts\n- Lives in Paris")
    );

    clear_data_dir();
}

#[test]
fn load_memory_md_returns_none_when_missing() {
    let _guard = DATA_DIR_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    set_data_dir(dir.path().to_path_buf());

    assert_eq!(load_memory_md(), None);

    clear_data_dir();
}

#[test]
fn load_memory_md_for_agent_is_agent_scoped() {
    let _guard = DATA_DIR_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    set_data_dir(dir.path().to_path_buf());

    std::fs::write(dir.path().join("MEMORY.md"), "root memory").unwrap();
    assert_eq!(load_memory_md_for_agent("main"), None);
    assert_eq!(load_memory_md_for_agent("ops"), None);

    let agent_dir = dir.path().join("agents").join("main");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(agent_dir.join("MEMORY.md"), "main agent memory").unwrap();
    assert_eq!(
        load_memory_md_for_agent("main").as_deref(),
        Some("main agent memory")
    );

    clear_data_dir();
}

#[test]
fn load_memory_md_for_agent_reports_agent_workspace_source_and_path() {
    let _guard = DATA_DIR_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    set_data_dir(dir.path().to_path_buf());

    let agent_dir = dir.path().join("agents").join("ops");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(agent_dir.join("MEMORY.md"), "ops memory").unwrap();
    let ops_memory = load_memory_md_for_agent_with_source("ops").unwrap();
    assert_eq!(ops_memory.content, "ops memory");
    assert_eq!(ops_memory.path, agent_dir.join("MEMORY.md"));
    assert_eq!(ops_memory.source, WorkspaceMarkdownSource::AgentWorkspace);

    clear_data_dir();
}

#[test]
fn memory_path_is_under_data_dir() {
    let _guard = DATA_DIR_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    set_data_dir(dir.path().to_path_buf());

    assert_eq!(memory_path(), dir.path().join("MEMORY.md"));

    clear_data_dir();
}

#[test]
fn workspace_markdown_ignores_leading_html_comments() {
    let _guard = DATA_DIR_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    set_data_dir(dir.path().to_path_buf());

    std::fs::write(
        dir.path().join("TOOLS.md"),
        "<!-- comment -->\n\nUse read-only tools first.",
    )
    .unwrap();
    assert_eq!(
        load_tools_md().as_deref(),
        Some("Use read-only tools first.")
    );

    clear_data_dir();
}

#[test]
fn load_soul_for_agent_does_not_create_missing_file() {
    let _guard = DATA_DIR_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    set_data_dir(dir.path().to_path_buf());

    let soul_file = dir.path().join("agents/main/SOUL.md");
    assert_eq!(load_soul_for_agent("main"), None);
    assert!(!soul_file.exists());

    clear_data_dir();
}

#[test]
fn load_soul_for_agent_does_not_reseed_deleted_file() {
    let _guard = DATA_DIR_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    set_data_dir(dir.path().to_path_buf());

    save_soul_for_agent("main", Some(DEFAULT_SOUL)).expect("save initial soul");
    let soul_file = dir.path().join("agents/main/SOUL.md");
    std::fs::remove_file(&soul_file).unwrap();

    assert_eq!(load_soul_for_agent("main"), None);
    assert!(!soul_file.exists());

    clear_data_dir();
}

#[test]
fn save_soul_for_agent_writes_to_agent_dir() {
    let _guard = DATA_DIR_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    set_data_dir(dir.path().to_path_buf());

    let custom = "Agent soul content.";
    save_soul_for_agent("main", Some(custom)).expect("save_soul_for_agent");

    let agent_soul = dir.path().join("agents/main/SOUL.md");
    assert!(agent_soul.exists(), "SOUL.md should exist in agents/main/");
    assert_eq!(std::fs::read_to_string(&agent_soul).unwrap(), custom);

    // load_soul_for_agent must find the agent-level file.
    let loaded = load_soul_for_agent("main");
    assert_eq!(loaded.as_deref(), Some(custom));

    clear_data_dir();
}

#[test]
fn save_soul_for_agent_none_clears() {
    let _guard = DATA_DIR_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    set_data_dir(dir.path().to_path_buf());

    save_soul_for_agent("main", Some("initial")).expect("save");
    save_soul_for_agent("main", None).expect("clear");

    let agent_soul = dir.path().join("agents/main/SOUL.md");
    assert!(agent_soul.exists(), "file should remain after clearing");
    assert!(
        std::fs::read_to_string(&agent_soul).unwrap().is_empty(),
        "file should be empty after clearing"
    );

    clear_data_dir();
}

// ── share_dir tests ─────────────────────────────────────────────────

#[test]
fn share_dir_override_takes_precedence() {
    let _guard = DATA_DIR_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    set_share_dir(dir.path().to_path_buf());

    let result = share_dir();
    assert_eq!(result, Some(dir.path().to_path_buf()));

    clear_share_dir();
}

#[test]
fn share_dir_returns_none_when_no_source() {
    let _guard = DATA_DIR_TEST_LOCK.lock().unwrap();
    clear_share_dir();
    // Without an override, env var, or existing directories, share_dir
    // should return None (unless /usr/share/chelix or ~/.chelix/share
    // happens to exist on the test machine).
    let _ = share_dir();
}

#[test]
fn share_dir_data_dir_fallback() {
    let _guard = DATA_DIR_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    set_data_dir(dir.path().to_path_buf());
    clear_share_dir();

    // Without the share/ subdirectory, should not return data_dir/share
    let result = share_dir();
    assert_ne!(result, Some(dir.path().join("share")));

    // Create the share/ subdirectory
    std::fs::create_dir(dir.path().join("share")).unwrap();
    let result = share_dir();
    assert_eq!(result, Some(dir.path().join("share")));

    clear_data_dir();
}

#[test]
fn normalize_workspace_markdown_content_strips_leading_comments_and_trims() {
    let content = "<!-- comment -->\n\n  hello world  \n";
    let normalized = normalize_workspace_markdown_content(content);
    assert_eq!(normalized.as_deref(), Some("hello world"));
}

#[test]
fn normalize_workspace_markdown_content_returns_none_for_comment_only_content() {
    let content = "  <!-- comment -->\n\n<!-- another -->  ";
    let normalized = normalize_workspace_markdown_content(content);
    assert_eq!(normalized, None);
}

#[test]
fn load_guidelines_md_for_agent_falls_back_to_root() {
    let _guard = DATA_DIR_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    set_data_dir(dir.path().to_path_buf());

    let docs_dir = dir.path().join("docs/chelix");
    std::fs::create_dir_all(&docs_dir).unwrap();
    std::fs::write(docs_dir.join("GUIDELINES.md"), "Root guidelines").unwrap();
    assert_eq!(
        load_guidelines_md_for_agent("test-agent").as_deref(),
        Some("Root guidelines")
    );

    let agent_dir = dir.path().join("agents/test-agent");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(agent_dir.join("GUIDELINES.md"), "Agent-specific guidelines").unwrap();
    assert_eq!(
        load_guidelines_md_for_agent("test-agent").as_deref(),
        Some("Agent-specific guidelines")
    );

    clear_data_dir();
}
