//! CLI subcommand for importing data from Claude Code and Claude Desktop.

use clap::Subcommand;

#[derive(Subcommand)]
pub enum ImportAction {
    /// Detect Claude data and show what can be imported.
    Detect {
        /// Emit structured JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Import all categories from Claude.
    All {
        /// Dry-run: show what would be imported without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Emit structured JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Import specific categories from Claude.
    Select {
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
        ImportAction::Detect { json } => handle_detect(json),
        ImportAction::All { dry_run, json } => handle_import_all(dry_run, json),
        ImportAction::Select {
            categories,
            dry_run,
            json,
        } => handle_import_select(&categories, dry_run, json),
    }
}

fn handle_detect(json_output: bool) -> anyhow::Result<()> {
    let mut results = serde_json::Map::new();
    let found = detect_claude(json_output, &mut results);

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::Value::Object(results))?
        );
    } else if !found {
        println!("No import sources detected.");
        println!("Checked: Claude Code (~/.claude/)");
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
        let Some(detection) = chelix_claude_import::detect::detect() else {
            if !json_output {
                println!("Claude Code: not detected");
            }
            results.insert("claude".to_string(), serde_json::json!({"detected": false}));
            return false;
        };

        let skills = chelix_claude_import::skills::discover_skills(&detection);
        let commands = chelix_claude_import::skills::discover_commands(&detection);

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

fn handle_import_all(dry_run: bool, json_output: bool) -> anyhow::Result<()> {
    if dry_run {
        return handle_detect(json_output);
    }

    let data_dir = chelix_config::data_dir();
    let mut results = serde_json::Map::new();
    import_claude_all(&data_dir, json_output, &mut results)?;

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::Value::Object(results))?
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
        let Some(detection) = chelix_claude_import::detect::detect() else {
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
            chelix_claude_import::mcp_servers::import_mcp_servers(&detection, &mcp_path),
            chelix_claude_import::skills::import_skills(&detection, &skills_dir),
            chelix_claude_import::memory::import_memory(&detection, data_dir),
        ];
        let total: usize = categories
            .iter()
            .map(|category| category.items_imported)
            .sum();

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

fn handle_import_select(
    categories: &[String],
    dry_run: bool,
    json_output: bool,
) -> anyhow::Result<()> {
    if dry_run {
        return handle_detect(json_output);
    }

    let data_dir = chelix_config::data_dir();
    import_claude_select(categories, &data_dir, json_output)
}

#[cfg_attr(not(feature = "claude-import"), allow(unused_variables))]
fn import_claude_select(
    categories: &[String],
    data_dir: &std::path::Path,
    json_output: bool,
) -> anyhow::Result<()> {
    #[cfg(feature = "claude-import")]
    {
        let Some(detection) = chelix_claude_import::detect::detect() else {
            anyhow::bail!("No Claude Code installation found");
        };

        let categories: Vec<String> = categories
            .iter()
            .map(|category| category.trim().to_lowercase())
            .collect();
        let mcp_path = data_dir.join("mcp-servers.json");
        let skills_dir = data_dir.join("skills");
        let mut reports = Vec::new();

        for category in &categories {
            match category.as_str() {
                "mcp_servers" | "mcp-servers" | "mcp" => {
                    reports.push(chelix_claude_import::mcp_servers::import_mcp_servers(
                        &detection, &mcp_path,
                    ));
                },
                "skills" | "commands" => {
                    reports.push(chelix_claude_import::skills::import_skills(
                        &detection,
                        &skills_dir,
                    ));
                },
                "memory" => {
                    reports.push(chelix_claude_import::memory::import_memory(
                        &detection, data_dir,
                    ));
                },
                other => eprintln!("Warning: unknown category '{other}' for claude, skipping"),
            }
        }

        if json_output {
            let total: usize = reports.iter().map(|category| category.items_imported).sum();
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
        Some(detail) if available => println!("  [{status}] {name}: {detail}"),
        _ => println!("  [{status}] {name}"),
    }
}

fn print_report(source: &str, categories: &[impl AsReport]) {
    println!();
    println!("{source} import complete:");
    for category in categories {
        let (name, status, imported, updated, skipped, warnings, errors) = category.as_report();
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
        for warning in warnings {
            println!("      warning: {warning}");
        }
        for error in errors {
            println!("      error: {error}");
        }
    }
    println!();
}

trait AsReport {
    fn as_report(&self) -> (&str, &str, usize, usize, usize, &[String], &[String]);
}

impl AsReport for chelix_import_core::report::CategoryReport {
    fn as_report(&self) -> (&str, &str, usize, usize, usize, &[String], &[String]) {
        let status = match self.status {
            chelix_import_core::report::ImportStatus::Success => "success",
            chelix_import_core::report::ImportStatus::Partial => "partial",
            chelix_import_core::report::ImportStatus::Skipped => "skipped",
            chelix_import_core::report::ImportStatus::Failed => "failed",
        };
        let name = match self.category {
            chelix_import_core::report::ImportCategory::Identity => "Identity",
            chelix_import_core::report::ImportCategory::Providers => "Providers",
            chelix_import_core::report::ImportCategory::Skills => "Skills",
            chelix_import_core::report::ImportCategory::Memory => "Memory",
            chelix_import_core::report::ImportCategory::Channels => "Channels",
            chelix_import_core::report::ImportCategory::Sessions => "Sessions",
            chelix_import_core::report::ImportCategory::McpServers => "MCP Servers",
            chelix_import_core::report::ImportCategory::WorkspaceFiles => "Workspace Files",
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
