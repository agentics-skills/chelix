//! MCP timeout, transport, and server-reference checks.

use std::collections::HashSet;

use chelix_config::{
    ChelixConfig, Diagnostic, DiagnosticCategory, Severity, schema::AgentMcpPolicy,
};

/// Report MCP timeouts, unknown transports, and unknown agent server references.
///
/// Agent allow/deny names are warnings when they are absent from both
/// `[mcp.servers]` and `data_dir()/mcp-servers.json`.
#[must_use]
pub fn validate_config(config: &ChelixConfig) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    check_timeouts_and_transports(config, &mut diagnostics);
    check_server_references(config, &mut diagnostics);
    diagnostics
}

fn check_timeouts_and_transports(config: &ChelixConfig, diagnostics: &mut Vec<Diagnostic>) {
    if config.mcp.request_timeout_secs == 0 {
        diagnostics.push(Diagnostic {
            severity: Severity::Error,
            summary_message: None,
            category: DiagnosticCategory::InvalidValue,
            path: "mcp.request_timeout_secs".into(),
            message: "mcp.request_timeout_secs must be at least 1".into(),
        });
    }
    for (name, server) in &config.mcp.servers {
        if server.request_timeout_secs == Some(0) {
            diagnostics.push(Diagnostic {
                severity: Severity::Error,
                summary_message: None,
                category: DiagnosticCategory::InvalidValue,
                path: format!("mcp.servers.{name}.request_timeout_secs"),
                message: "mcp server request_timeout_secs must be at least 1".into(),
            });
        }
        if !server.transport.is_empty()
            && !matches!(
                server.transport.as_str(),
                "stdio" | "sse" | "streamable-http" | "streamable_http" | "http"
            )
        {
            diagnostics.push(Diagnostic {
                severity: Severity::Warning,
                summary_message: None,
                category: DiagnosticCategory::InvalidValue,
                path: format!("mcp.servers.{name}.transport"),
                message: format!(
                    "unknown transport type \"{}\"; expected \"stdio\", \"sse\", or \"streamable-http\"",
                    server.transport
                ),
            });
        }
    }
}

fn check_server_references(config: &ChelixConfig, diagnostics: &mut Vec<Diagnostic>) {
    let mut known_mcp_servers: HashSet<String> = config
        .mcp
        .servers
        .keys()
        .map(|name| name.as_str().to_string())
        .collect();
    let registry_path = chelix_config::data_dir().join("mcp-servers.json");
    if let Ok(data) = std::fs::read_to_string(&registry_path)
        && let Ok(json) = serde_json::from_str::<serde_json::Value>(&data)
        && let Some(servers) = json.get("servers").and_then(serde_json::Value::as_object)
    {
        known_mcp_servers.extend(servers.keys().cloned());
    }
    for (agent_id, agent) in &config.agents.entries {
        let servers = match &agent.mcp {
            AgentMcpPolicy::Allow(servers) | AgentMcpPolicy::Deny(servers) => servers,
            AgentMcpPolicy::All => continue,
        };
        for server in servers {
            if !known_mcp_servers.contains(server.as_str()) {
                diagnostics.push(Diagnostic {
                    severity: Severity::Warning,
                    summary_message: None,
                    category: DiagnosticCategory::Agents,
                    path: format!("agents.{agent_id}.mcp"),
                    message: format!(
                        "MCP server '{}' referenced by agent but not configured in [mcp.servers] or mcp-servers.json",
                        server
                    ),
                });
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::HashMap;

    use chelix_config::{
        AgentConfig,
        schema::{McpServerEntry, McpServerId, ReasoningEffort},
    };

    use super::*;

    fn check(config: &ChelixConfig) -> Vec<Diagnostic> {
        validate_config(config)
    }

    fn server(timeout: Option<u64>, transport: &str) -> McpServerEntry {
        McpServerEntry {
            command: "npx".into(),
            args: Vec::new(),
            env: HashMap::new(),
            enabled: true,
            request_timeout_secs: timeout,
            transport: transport.into(),
            url: None,
            headers: HashMap::new(),
            oauth: None,
            display_name: None,
        }
    }

    struct DataDirGuard;

    impl Drop for DataDirGuard {
        fn drop(&mut self) {
            chelix_config::clear_data_dir();
        }
    }

    #[test]
    fn default_timeout_is_accepted() {
        assert!(check(&ChelixConfig::default()).is_empty());
    }

    #[test]
    fn mcp_request_timeout_must_be_positive() {
        let mut config = ChelixConfig::default();
        config.mcp.request_timeout_secs = 0;
        assert!(check(&config).iter().any(|diagnostic| {
            diagnostic.path == "mcp.request_timeout_secs"
                && diagnostic.severity == Severity::Error
                && diagnostic.category == DiagnosticCategory::InvalidValue
        }));
    }

    #[test]
    fn mcp_server_request_timeout_override_must_be_positive() {
        let mut config = ChelixConfig::default();
        config
            .mcp
            .servers
            .insert(McpServerId::new("memory"), server(Some(0), "stdio"));
        assert!(check(&config).iter().any(|diagnostic| {
            diagnostic.path == "mcp.servers.memory.request_timeout_secs"
                && diagnostic.severity == Severity::Error
                && diagnostic.category == DiagnosticCategory::InvalidValue
        }));

        let mut positive = ChelixConfig::default();
        positive
            .mcp
            .servers
            .insert(McpServerId::new("memory"), server(Some(5), ""));
        assert!(
            check(&positive)
                .iter()
                .all(|diagnostic| diagnostic.path != "mcp.servers.memory.request_timeout_secs")
        );
    }

    #[test]
    fn known_transports_are_accepted_and_unknown_transport_is_warned() {
        for transport in [
            "stdio",
            "sse",
            "streamable-http",
            "streamable_http",
            "http",
            "",
        ] {
            let mut config = ChelixConfig::default();
            config
                .mcp
                .servers
                .insert(McpServerId::new("memory"), server(None, transport));
            assert!(
                check(&config)
                    .iter()
                    .all(|diagnostic| diagnostic.path != "mcp.servers.memory.transport"),
                "{transport}"
            );
        }
        let mut config = ChelixConfig::default();
        config
            .mcp
            .servers
            .insert(McpServerId::new("memory"), server(None, "carrier-pigeon"));
        assert!(check(&config).iter().any(|diagnostic| {
            diagnostic.path == "mcp.servers.memory.transport"
                && diagnostic.severity == Severity::Warning
                && diagnostic.message.contains("carrier-pigeon")
        }));
    }

    #[test]
    fn agent_mcp_server_references_use_config_and_registry() {
        let dir = tempfile::tempdir().unwrap();
        chelix_config::set_data_dir(dir.path().to_path_buf());
        let _guard = DataDirGuard;
        std::fs::write(
            dir.path().join("mcp-servers.json"),
            r#"{"servers":{"from-registry":{"command":"npx"}}}"#,
        )
        .unwrap();

        let mut agent = AgentConfig::new("Main", "test::model", ReasoningEffort::from("off"));
        agent.mcp = AgentMcpPolicy::Allow(vec![
            McpServerId::new("from-toml"),
            McpServerId::new("from-registry"),
            McpServerId::new("missing-server"),
        ]);
        let mut config = ChelixConfig::default();
        config
            .mcp
            .servers
            .insert(McpServerId::new("from-toml"), server(None, "stdio"));
        config.agents.entries.insert("main".into(), agent);

        let diagnostics = check(&config);
        let warnings: Vec<_> = diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.category == DiagnosticCategory::Agents)
            .collect();
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].path, "agents.main.mcp");
        assert_eq!(warnings[0].severity, Severity::Warning);
        assert!(warnings[0].message.contains("missing-server"));
    }
}
