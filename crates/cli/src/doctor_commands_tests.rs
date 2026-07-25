use {
    super::*,
    chelix_config::{ChelixConfig, validate::Diagnostic},
};

#[test]
fn status_labels() {
    assert_eq!(Status::Ok.label(), "ok");
    assert_eq!(Status::Warn.label(), "warn");
    assert_eq!(Status::Fail.label(), "fail");
    assert_eq!(Status::Skip.label(), "skip");
    assert_eq!(Status::Info.label(), "info");
}

#[test]
fn section_push_counts() {
    let mut section = Section::new("test");
    section.push(Status::Ok, "good");
    section.push(Status::Warn, "attention");
    section.push(Status::Fail, "bad");
    assert_eq!(section.items.len(), 3);
    assert_eq!(section.items[0].status, Status::Ok);
    assert_eq!(section.items[1].status, Status::Warn);
    assert_eq!(section.items[2].status, Status::Fail);
}

#[test]
fn print_report_counts_errors_and_warnings() {
    let mut section = Section::new("test");
    section.push(Status::Ok, "fine");
    section.push(Status::Warn, "caution");
    section.push(Status::Warn, "caution2");
    section.push(Status::Fail, "broken");
    section.push(Status::Info, "note");

    let (errors, warnings) = print_report(&[section]);
    assert_eq!(errors, 1);
    assert_eq!(warnings, 2);
}

#[test]
fn config_dependent_section_reports_explicit_skip() {
    let section = skipped_config_section("Providers", "effective config could not be loaded");

    assert_eq!(section.title, "Providers");
    assert_eq!(section.items.len(), 1);
    assert_eq!(section.items[0].status, Status::Skip);
    assert_eq!(
        section.items[0].message,
        "Skipped: effective config could not be loaded"
    );
}

#[test]
fn security_check_continues_without_effective_config() {
    let temp = tempfile::TempDir::new().unwrap();
    let section = check_security(None, Some(temp.path()), temp.path());

    assert!(section.items.iter().any(|item| {
        item.status == Status::Skip
            && item
                .message
                .contains("effective config could not be loaded")
    }));
}

#[test]
fn config_validation_status_warns_for_deprecated_field() {
    let diagnostic = Diagnostic {
        severity: Severity::Warning,
        category: "deprecated-field",
        path: "memory.embedding_provider".into(),
        message: "deprecated field; use \"memory.provider\" instead".into(),
    };

    assert_eq!(config_validation_status(&diagnostic), Some(Status::Warn));
}

#[test]
fn check_providers_empty_config() {
    let config = ChelixConfig::default();
    let section = check_providers(&config);
    assert_eq!(section.items.len(), 1);
    assert_eq!(section.items[0].status, Status::Info);
    assert!(section.items[0].message.contains("No providers configured"));
}

#[test]
fn check_providers_with_config_key() {
    let mut config = ChelixConfig::default();
    let entry = chelix_config::schema::ProviderEntry {
        api_key: Some(secrecy::Secret::new("sk-test-fake".to_string())),
        ..Default::default()
    };
    config.providers.providers.insert("anthropic".into(), entry);

    let section = check_providers(&config);
    let anthropic_item = section
        .items
        .iter()
        .find(|i| i.message.contains("anthropic"));
    assert!(anthropic_item.is_some());
    assert_eq!(anthropic_item.unwrap().status, Status::Ok);
}

#[test]
fn check_providers_missing_key_warns() {
    let mut config = ChelixConfig::default();
    config.providers.providers.insert(
        "openrouter".to_string(),
        chelix_config::schema::ProviderEntry::default(),
    );

    if std::env::var("OPENROUTER_API_KEY").is_err() {
        let section = check_providers(&config);
        let item = section
            .items
            .iter()
            .find(|i| i.message.contains("openrouter"));
        assert!(item.is_some());
        assert_eq!(item.unwrap().status, Status::Warn);
    }
}

#[test]
fn check_providers_disabled_skipped() {
    let mut config = ChelixConfig::default();
    let entry = chelix_config::schema::ProviderEntry {
        enabled: false,
        ..Default::default()
    };
    config.providers.providers.insert("openai".into(), entry);

    let section = check_providers(&config);
    let openai_item = section.items.iter().find(|i| i.message.contains("openai"));
    assert!(openai_item.is_some());
    assert_eq!(openai_item.unwrap().status, Status::Skip);
}

#[test]
fn check_providers_oauth_skipped() {
    let mut config = ChelixConfig::default();
    config.providers.providers.insert(
        "github-copilot".to_string(),
        chelix_config::schema::ProviderEntry::default(),
    );

    let section = check_providers(&config);
    let gh_item = section
        .items
        .iter()
        .find(|i| i.message.contains("github-copilot"));
    assert!(gh_item.is_some());
    assert_eq!(gh_item.unwrap().status, Status::Skip);
}

#[test]
fn check_mcp_servers_empty() {
    let config = ChelixConfig::default();
    let section = check_mcp_servers(&config);
    assert_eq!(section.items.len(), 1);
    assert_eq!(section.items[0].status, Status::Info);
}

#[test]
fn check_mcp_servers_disabled_skipped() {
    let mut config = ChelixConfig::default();
    let entry = chelix_config::schema::McpServerEntry {
        command: "node".to_string(),
        args: vec![],
        env: Default::default(),
        headers: Default::default(),
        enabled: false,
        transport: String::new(),
        url: None,
        oauth: None,
        display_name: None,
        request_timeout_secs: None,
    };
    config.mcp.servers.insert("test".into(), entry);

    let section = check_mcp_servers(&config);
    let test_item = section.items.iter().find(|i| i.message.contains("test"));
    assert!(test_item.is_some());
    assert_eq!(test_item.unwrap().status, Status::Skip);
}

#[test]
fn check_mcp_servers_missing_command_fails() {
    let mut config = ChelixConfig::default();
    let entry = chelix_config::schema::McpServerEntry {
        command: String::new(),
        args: vec![],
        env: Default::default(),
        headers: Default::default(),
        enabled: true,
        transport: String::new(),
        url: None,
        oauth: None,
        display_name: None,
        request_timeout_secs: None,
    };
    config.mcp.servers.insert("broken".into(), entry);

    let section = check_mcp_servers(&config);
    let broken_item = section.items.iter().find(|i| i.message.contains("broken"));
    assert!(broken_item.is_some());
    assert_eq!(broken_item.unwrap().status, Status::Fail);
}

#[test]
fn check_mcp_servers_sse_with_url_ok() {
    let mut config = ChelixConfig::default();
    let entry = chelix_config::schema::McpServerEntry {
        command: String::new(),
        args: vec![],
        env: Default::default(),
        headers: Default::default(),
        enabled: true,
        transport: "sse".to_string(),
        url: Some("http://localhost:3000/sse".to_string()),
        oauth: None,
        display_name: None,
        request_timeout_secs: None,
    };
    config.mcp.servers.insert("remote".into(), entry);

    let section = check_mcp_servers(&config);
    let remote_item = section.items.iter().find(|i| i.message.contains("remote"));
    assert!(remote_item.is_some());
    assert_eq!(remote_item.unwrap().status, Status::Ok);
}

#[test]
fn check_mcp_servers_sse_without_url_fails() {
    let mut config = ChelixConfig::default();
    let entry = chelix_config::schema::McpServerEntry {
        command: String::new(),
        args: vec![],
        env: Default::default(),
        headers: Default::default(),
        enabled: true,
        transport: "sse".to_string(),
        url: None,
        oauth: None,
        display_name: None,
        request_timeout_secs: None,
    };
    config.mcp.servers.insert("broken-sse".into(), entry);

    let section = check_mcp_servers(&config);
    let item = section
        .items
        .iter()
        .find(|i| i.message.contains("broken-sse"));
    assert!(item.is_some());
    assert_eq!(item.unwrap().status, Status::Fail);
}

#[test]
fn check_mcp_servers_nonexistent_command_fails() {
    let mut config = ChelixConfig::default();
    let entry = chelix_config::schema::McpServerEntry {
        command: "definitely-not-a-real-command-xyz123".to_string(),
        args: vec![],
        env: Default::default(),
        headers: Default::default(),
        enabled: true,
        transport: String::new(),
        url: None,
        oauth: None,
        display_name: None,
        request_timeout_secs: None,
    };
    config.mcp.servers.insert("bad".into(), entry);

    let section = check_mcp_servers(&config);
    let item = section.items.iter().find(|i| i.message.contains("bad"));
    assert!(item.is_some());
    assert_eq!(item.unwrap().status, Status::Fail);
}

#[test]
fn check_directories_with_temp_dirs() {
    let temp = tempfile::TempDir::new().unwrap();
    let config_dir = temp.path().join("config");
    let data_dir = temp.path().join("data");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::create_dir_all(&data_dir).unwrap();

    let section = check_directories(Some(&config_dir), &data_dir);

    let ok_count = section
        .items
        .iter()
        .filter(|i| i.status == Status::Ok)
        .count();
    assert!(
        ok_count >= 2,
        "expected at least 2 OK items, got {ok_count}"
    );
}

#[test]
fn check_directories_missing_config_dir() {
    let temp = tempfile::TempDir::new().unwrap();
    let missing = temp.path().join("nonexistent");
    let data_dir = temp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let section = check_directories(Some(&missing), &data_dir);

    let fail_item = section
        .items
        .iter()
        .find(|i| i.status == Status::Fail && i.message.contains("Config directory missing"));
    assert!(fail_item.is_some());
}

#[tokio::test]
async fn check_database_missing_file() {
    let temp = tempfile::TempDir::new().unwrap();
    let section = check_database(temp.path()).await;
    assert_eq!(section.items.len(), 1);
    assert_eq!(section.items[0].status, Status::Skip);
}

#[tokio::test]
async fn check_database_valid_db() {
    let temp = tempfile::TempDir::new().unwrap();
    let db_path = temp.path().join("chelix.db");
    let db_url = format!("sqlite:{}?mode=rwc", db_path.display());
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&db_url)
        .await
        .unwrap();
    pool.close().await;

    let section = check_database(temp.path()).await;
    let ok_item = section.items.iter().find(|i| i.status == Status::Ok);
    assert!(
        ok_item.is_some(),
        "expected OK for valid db, got: {:?}",
        section
            .items
            .iter()
            .map(|i| (&i.status, &i.message))
            .collect::<Vec<_>>()
    );
}

#[test]
fn check_security_no_api_keys_in_config() {
    let config = ChelixConfig::default();
    let temp = tempfile::TempDir::new().unwrap();
    let section = check_security(Some(&config), Some(temp.path()), temp.path());

    let ok_item = section
        .items
        .iter()
        .find(|i| i.message.contains("No API keys in config file"));
    assert!(ok_item.is_some());
    assert_eq!(ok_item.unwrap().status, Status::Ok);
}

#[test]
fn check_security_api_keys_in_config_warns() {
    let mut config = ChelixConfig::default();
    let entry = chelix_config::schema::ProviderEntry {
        api_key: Some(secrecy::Secret::new("sk-test".to_string())),
        ..Default::default()
    };
    config.providers.providers.insert("anthropic".into(), entry);

    let temp = tempfile::TempDir::new().unwrap();
    let section = check_security(Some(&config), Some(temp.path()), temp.path());

    let warn_item = section
        .items
        .iter()
        .find(|i| i.message.contains("API keys found in config"));
    assert!(warn_item.is_some());
    assert_eq!(warn_item.unwrap().status, Status::Warn);
}
