//! CLI subcommand for importing data from external AI tools.

use clap::Subcommand;

#[derive(clap::ValueEnum, Clone, Debug)]
pub enum ImportSource {
    /// Import from Claude Code and Claude Desktop.
    Claude,
    /// Import from Codex CLI.
    Codex,
}

#[derive(Subcommand)]
pub enum ImportAction {
    /// Detect available import sources and show what can be imported.
    Detect {
        /// Only detect a specific source.
        #[arg(short, long)]
        source: Option<ImportSource>,
        /// Emit structured JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Import all categories from detected sources.
    All {
        /// Only import from a specific source.
        #[arg(short, long)]
        source: Option<ImportSource>,
        /// Dry-run: show what would be imported without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Emit structured JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Import specific categories from a source.
    Select {
        /// Source to import from (required for selective import).
        #[arg(short, long)]
        source: ImportSource,
        /// Comma-separated list of categories to import.
        #[arg(short, long, value_delimiter = ',')]
        categories: Vec<String>,
        /// Dry-run: show what would be imported without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Emit structured JSON output.
        #[arg(long)]
        json: bool,
    },
}

pub async fn handle_import(action: ImportAction) -> anyhow::Result<()> {
    match action {
        ImportAction::Detect { source, json } => handle_detect(source, json),
        ImportAction::All {
            source,
            dry_run,
            json,
        } => handle_import_all(source, dry_run, json),
        ImportAction::Select {
            source,
            categories,
            dry_run,
            json,
        } => handle_import_select(source, &categories, dry_run, json),
    }
}

// ── Detection ────────────────────────────────────────────────────────────────

fn handle_detect(source: Option<ImportSource>, json_output: bool) -> anyhow::Result<()> {
    let mut results = serde_json::Map::new();
    let mut any_found = false;

    if source.is_none() || matches!(source, Some(ImportSource::Claude)) {
        let found = detect_claude(json_output, &mut results);
        any_found |= found;
    }

    if source.is_none() || matches!(source, Some(ImportSource::Codex)) {
        let found = detect_codex(json_output, &mut results);
        any_found |= found;
    }

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::Value::Object(results))?
        );
    } else if !any_found {
        println!("No import sources detected.");
        println!("Checked: Claude Code (~/.claude/), Codex CLI (~/.codex/)");
    }

    Ok(())
}

#[cfg_attr(not(feature = "claude-import"), allow(unused_variables))]
fn detect_claude(
    json_output: bool,
    results: &mut serde_json::Map<String, serde_json::Value>,
) -> bool {
    #[cfg(feature = "claude-import")]
    {
        let Some(detection) = moltis_claude_import::detect::detect() else {
            if !json_output {
                println!("Claude Code: not detected");
            }
            results.insert("claude".to_string(), serde_json::json!({"detected": false}));
            return false;
        };

        let skills = moltis_claude_import::skills::discover_skills(&detection);
        let commands = moltis_claude_import::skills::discover_commands(&detection);

        if json_output {
            results.insert(
                "claude".to_string(),
                serde_json::json!({
                    "detected": true,
                    "has_settings": detection.user_settings_path.is_some(),
                    "has_claude_json": detection.user_claude_json_path.is_some(),
                    "has_desktop_config": detection.desktop_config_path.is_some(),
                    "skills_count": skills.len(),
                    "commands_count": commands.len(),
                    "has_memory": detection.user_memory_path.is_some(),
                }),
            );
        } else {
            println!("Claude Code: detected");
            print_scan_item(
                "  MCP Servers",
                detection.user_claude_json_path.is_some()
                    || detection.desktop_config_path.is_some(),
                None,
            );
            print_scan_item(
                "  Skills",
                !skills.is_empty(),
                Some(format!("{} skill(s)", skills.len())),
            );
            print_scan_item(
                "  Commands",
                !commands.is_empty(),
                Some(format!("{} command(s) -> skills", commands.len())),
            );
            print_scan_item("  Memory", detection.user_memory_path.is_some(), None);
            println!();
        }
        true
    }
    #[cfg(not(feature = "claude-import"))]
    {
        false
    }
}

#[cfg_attr(not(feature = "codex-import"), allow(unused_variables))]
fn detect_codex(
    json_output: bool,
    results: &mut serde_json::Map<String, serde_json::Value>,
) -> bool {
    #[cfg(feature = "codex-import")]
    {
        let Some(detection) = moltis_codex_import::detect::detect() else {
            if !json_output {
                println!("Codex CLI: not detected");
            }
            results.insert("codex".to_string(), serde_json::json!({"detected": false}));
            return false;
        };

        let mcp_count = moltis_codex_import::mcp_servers::count_mcp_servers(&detection);

        if json_output {
            results.insert(
                "codex".to_string(),
                serde_json::json!({
                    "detected": true,
                    "home_dir": detection.home_dir.display().to_string(),
                    "mcp_servers_count": mcp_count,
                    "has_memory": detection.instructions_path.is_some(),
                }),
            );
        } else {
            println!("Codex CLI: detected at {}", detection.home_dir.display());
            print_scan_item(
                "  MCP Servers",
                mcp_count > 0,
                Some(format!("{mcp_count} server(s)")),
            );
            print_scan_item("  Memory", detection.instructions_path.is_some(), None);
            println!();
        }
        true
    }
    #[cfg(not(feature = "codex-import"))]
    {
        false
    }
}

// ── Import All ───────────────────────────────────────────────────────────────

fn handle_import_all(
    source: Option<ImportSource>,
    dry_run: bool,
    json_output: bool,
) -> anyhow::Result<()> {
    if dry_run {
        return handle_detect(source, json_output);
    }

    let data_dir = moltis_config::data_dir();

    let mut all_results = serde_json::Map::new();

    if source.is_none() || matches!(source, Some(ImportSource::Claude)) {
        import_claude_all(&data_dir, json_output, &mut all_results)?;
    }

    if source.is_none() || matches!(source, Some(ImportSource::Codex)) {
        import_codex_all(&data_dir, json_output, &mut all_results)?;
    }

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::Value::Object(all_results))?
        );
    }

    Ok(())
}

#[cfg_attr(not(feature = "claude-import"), allow(unused_variables))]
fn import_claude_all(
    data_dir: &std::path::Path,
    json_output: bool,
    results: &mut serde_json::Map<String, serde_json::Value>,
) -> anyhow::Result<()> {
    #[cfg(feature = "claude-import")]
    {
        let Some(detection) = moltis_claude_import::detect::detect() else {
            if !json_output {
                println!("Claude Code: not detected, skipping");
            }
            return Ok(());
        };

        if !json_output {
            println!("Importing from Claude Code ...");
        }

        let mcp_path = data_dir.join("mcp-servers.json");
        let skills_dir = data_dir.join("skills");

        let categories = vec![
            moltis_claude_import::mcp_servers::import_mcp_servers(&detection, &mcp_path),
            moltis_claude_import::skills::import_skills(&detection, &skills_dir),
            moltis_claude_import::memory::import_memory(&detection, data_dir),
        ];

        let total: usize = categories.iter().map(|c| c.items_imported).sum();

        if json_output {
            results.insert(
                "claude".to_string(),
                serde_json::json!({
                    "categories": categories,
                    "total_imported": total,
                }),
            );
        } else {
            print_report("Claude Code", &categories);
        }
    }
    Ok(())
}

#[cfg_attr(not(feature = "codex-import"), allow(unused_variables))]
fn import_codex_all(
    data_dir: &std::path::Path,
    json_output: bool,
    results: &mut serde_json::Map<String, serde_json::Value>,
) -> anyhow::Result<()> {
    #[cfg(feature = "codex-import")]
    {
        let Some(detection) = moltis_codex_import::detect::detect() else {
            if !json_output {
                println!("Codex CLI: not detected, skipping");
            }
            return Ok(());
        };

        if !json_output {
            println!(
                "Importing from Codex CLI at {} ...",
                detection.home_dir.display()
            );
        }

        let mcp_path = data_dir.join("mcp-servers.json");

        let categories = vec![
            moltis_codex_import::mcp_servers::import_mcp_servers(&detection, &mcp_path),
            moltis_codex_import::memory::import_memory(&detection, data_dir),
        ];

        let total: usize = categories.iter().map(|c| c.items_imported).sum();

        if json_output {
            results.insert(
                "codex".to_string(),
                serde_json::json!({
                    "categories": categories,
                    "total_imported": total,
                }),
            );
        } else {
            print_report("Codex CLI", &categories);
        }
    }
    Ok(())
}

// ── Selective Import ─────────────────────────────────────────────────────────

fn handle_import_select(
    source: ImportSource,
    categories: &[String],
    dry_run: bool,
    json_output: bool,
) -> anyhow::Result<()> {
    if dry_run {
        return handle_detect(Some(source), json_output);
    }

    let data_dir = moltis_config::data_dir();

    match source {
        ImportSource::Claude => import_claude_select(categories, &data_dir, json_output),
        ImportSource::Codex => import_codex_select(categories, &data_dir, json_output),
    }
}

#[cfg_attr(not(feature = "claude-import"), allow(unused_variables))]
fn import_claude_select(
    categories: &[String],
    data_dir: &std::path::Path,
    json_output: bool,
) -> anyhow::Result<()> {
    #[cfg(feature = "claude-import")]
    {
        let Some(detection) = moltis_claude_import::detect::detect() else {
            anyhow::bail!("No Claude Code installation found");
        };

        let cats: Vec<String> = categories.iter().map(|c| c.trim().to_lowercase()).collect();

        let mcp_path = data_dir.join("mcp-servers.json");
        let skills_dir = data_dir.join("skills");

        let mut reports = Vec::new();
        for cat in &cats {
            match cat.as_str() {
                "mcp_servers" | "mcp-servers" | "mcp" => {
                    reports.push(moltis_claude_import::mcp_servers::import_mcp_servers(
                        &detection, &mcp_path,
                    ));
                },
                "skills" | "commands" => {
                    reports.push(moltis_claude_import::skills::import_skills(
                        &detection,
                        &skills_dir,
                    ));
                },
                "memory" => {
                    reports.push(moltis_claude_import::memory::import_memory(
                        &detection, data_dir,
                    ));
                },
                other => eprintln!("Warning: unknown category '{other}' for claude, skipping"),
            }
        }

        if json_output {
            let total: usize = reports.iter().map(|c| c.items_imported).sum();
            print_json(serde_json::json!({
                "source": "claude",
                "categories": reports,
                "total_imported": total,
            }))?;
        } else {
            print_report("Claude Code", &reports);
        }
    }
    #[cfg(not(feature = "claude-import"))]
    anyhow::bail!("claude-import feature is not enabled");
    #[allow(unreachable_code)]
    Ok(())
}

#[cfg_attr(not(feature = "codex-import"), allow(unused_variables))]
fn import_codex_select(
    categories: &[String],
    data_dir: &std::path::Path,
    json_output: bool,
) -> anyhow::Result<()> {
    #[cfg(feature = "codex-import")]
    {
        let Some(detection) = moltis_codex_import::detect::detect() else {
            anyhow::bail!("No Codex CLI installation found");
        };

        let cats: Vec<String> = categories.iter().map(|c| c.trim().to_lowercase()).collect();
        let mcp_path = data_dir.join("mcp-servers.json");

        let mut reports = Vec::new();
        for cat in &cats {
            match cat.as_str() {
                "mcp_servers" | "mcp-servers" | "mcp" => {
                    reports.push(moltis_codex_import::mcp_servers::import_mcp_servers(
                        &detection, &mcp_path,
                    ));
                },
                "memory" => {
                    reports.push(moltis_codex_import::memory::import_memory(
                        &detection, data_dir,
                    ));
                },
                other => eprintln!("Warning: unknown category '{other}' for codex, skipping"),
            }
        }

        if json_output {
            let total: usize = reports.iter().map(|c| c.items_imported).sum();
            print_json(serde_json::json!({
                "source": "codex",
                "categories": reports,
                "total_imported": total,
            }))?;
        } else {
            print_report("Codex CLI", &reports);
        }
    }
    #[cfg(not(feature = "codex-import"))]
    anyhow::bail!("codex-import feature is not enabled");
    #[allow(unreachable_code)]
    Ok(())
}

// ── Output helpers ───────────────────────────────────────────────────────────

fn print_json(value: serde_json::Value) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn print_scan_item(name: &str, available: bool, detail: Option<String>) {
    let status = if available {
        "+"
    } else {
        "-"
    };
    match detail {
        Some(d) if available => println!("  [{status}] {name}: {d}"),
        _ => println!("  [{status}] {name}"),
    }
}

fn print_report(source: &str, categories: &[impl AsReport]) {
    println!();
    println!("{source} import complete:");
    for cat in categories {
        let (name, status, imported, updated, skipped, warnings, errors) = cat.as_report();
        let icon = match status {
            "success" => "+",
            "partial" => "~",
            "skipped" => "-",
            _ => "!",
        };
        if updated > 0 {
            println!(
                "  [{icon}] {name}: {imported} imported, {updated} updated, {skipped} skipped"
            );
        } else {
            println!("  [{icon}] {name}: {imported} imported, {skipped} skipped");
        }
        for w in warnings {
            println!("      warning: {w}");
        }
        for e in errors {
            println!("      error: {e}");
        }
    }
    println!();
}

/// Trait to unify report printing across different report types.
trait AsReport {
    fn as_report(&self) -> (&str, &str, usize, usize, usize, &[String], &[String]);
}

impl AsReport for moltis_import_core::report::CategoryReport {
    fn as_report(&self) -> (&str, &str, usize, usize, usize, &[String], &[String]) {
        let status = match self.status {
            moltis_import_core::report::ImportStatus::Success => "success",
            moltis_import_core::report::ImportStatus::Partial => "partial",
            moltis_import_core::report::ImportStatus::Skipped => "skipped",
            moltis_import_core::report::ImportStatus::Failed => "failed",
        };
        let name = match self.category {
            moltis_import_core::report::ImportCategory::Identity => "Identity",
            moltis_import_core::report::ImportCategory::Providers => "Providers",
            moltis_import_core::report::ImportCategory::Skills => "Skills",
            moltis_import_core::report::ImportCategory::Memory => "Memory",
            moltis_import_core::report::ImportCategory::Channels => "Channels",
            moltis_import_core::report::ImportCategory::Sessions => "Sessions",
            moltis_import_core::report::ImportCategory::McpServers => "MCP Servers",
            moltis_import_core::report::ImportCategory::WorkspaceFiles => "Workspace Files",
        };
        (
            name,
            status,
            self.items_imported,
            self.items_updated,
            self.items_skipped,
            &self.warnings,
            &self.errors,
        )
    }
}
