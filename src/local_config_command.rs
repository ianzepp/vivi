//! `vivi config show | validate | migrate`.
#![allow(clippy::missing_errors_doc)]

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use chrono::Utc;
use vivi::VivariumError;
use vivi::cli::ConfigCommand;
use vivi::mailspace::Mailspace;
use vivi::mailspace::config_edit;
use vivi::routing::{
    apply_plan_to_document, format_report, overview, plan_migrate, render_plan, render_text,
    require_ok, unified_diff, validate,
};

pub(crate) fn handle_config_command(command: &ConfigCommand) -> Result<(), VivariumError> {
    match command {
        ConfigCommand::Show {
            resolved,
            json,
            porcelain,
            project,
        } => show(*resolved, *json, *porcelain, project.as_deref()),
        ConfigCommand::Validate { json, project } => validate_cmd(*json, project.as_deref()),
        ConfigCommand::Migrate { write, project } => migrate(*write, project.as_deref()),
    }
}

fn show(
    resolved: bool,
    json: bool,
    porcelain: bool,
    project: Option<&Path>,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let free = resolved.then(|| live_free(&mailspace)).transpose()?;
    let view = overview(&mailspace.config, free.as_ref());
    if json {
        println!("{}", serde_json::to_string_pretty(&view).map_err(json_err)?);
        return Ok(());
    }
    if porcelain {
        print_porcelain(&view);
        return Ok(());
    }
    println!("{}", render_text(&view));
    Ok(())
}

fn print_porcelain(view: &vivi::routing::ConfigOverview) {
    for row in &view.roles {
        println!(
            "role {} {} {} {} {} {} {}",
            row.role,
            row.kind,
            row.pool,
            row.slots,
            row.models.replace(' ', ","),
            if row.read_only { "read-only" } else { "write" },
            row.source
        );
    }
    let flow = &view.flow;
    println!(
        "flow {} {} {} {} {} {} {} {}",
        flow.merge_role,
        flow.merge_pool,
        flow.merge_lanes,
        flow.gate_role,
        flow.gate_pool,
        flow.branch,
        if flow.promote { "promote" } else { "hold" },
        flow.main
    );
}

fn validate_cmd(json: bool, project: Option<&Path>) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let issues = validate(&mailspace.config);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&issues_json(&issues)).map_err(json_err)?
        );
    } else {
        let report = format_report(&issues);
        if !report.is_empty() {
            println!("{report}");
        }
    }
    require_ok(&issues)
}

fn issues_json(issues: &[vivi::routing::Issue]) -> serde_json::Value {
    serde_json::json!(
        issues
            .iter()
            .map(|issue| {
                serde_json::json!({
                    "severity": match issue.severity {
                        vivi::routing::Severity::Error => "error",
                        vivi::routing::Severity::Warning => "warning",
                    },
                    "message": issue.message,
                })
            })
            .collect::<Vec<_>>()
    )
}

fn migrate(write: bool, project: Option<&Path>) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let path = mailspace.config_path();
    let before = fs::read_to_string(&path)?;
    let plan = plan_migrate(&mailspace.config);
    println!("{}", render_plan(&plan));
    let mut doc = config_edit::load(&path)?;
    apply_plan_to_document(&mut doc, &plan)?;
    let after = doc.to_string();
    print!("{}", unified_diff(&before, &after, "mailspace.toml"));
    if !write {
        println!("dry-run; pass --write to apply (a timestamped .bak is kept)");
        return Ok(());
    }
    write_backup(&path, &before)?;
    config_edit::write_text(&path, &after)?;
    println!("wrote {} (backup beside it)", path.display());
    Ok(())
}

fn write_backup(path: &Path, before: &str) -> Result<(), VivariumError> {
    let stamp = Utc::now().format("%Y%m%dT%H%M%S");
    let backup = path.with_file_name(format!(
        "{}.bak.{stamp}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("mailspace.toml")
    ));
    fs::write(&backup, before)?;
    println!("backup {}", backup.display());
    Ok(())
}

fn live_free(mailspace: &Mailspace) -> Result<BTreeMap<String, u32>, VivariumError> {
    let out = std::process::Command::new(std::env::current_exe().unwrap_or_else(|_| "vivi".into()))
        .args(["pool", "status", "--porcelain"])
        .current_dir(&mailspace.root)
        .output();
    let Ok(out) = out else {
        return Ok(BTreeMap::new());
    };
    Ok(count_free(&String::from_utf8_lossy(&out.stdout)))
}

fn count_free(text: &str) -> BTreeMap<String, u32> {
    let mut free = BTreeMap::new();
    for line in text.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.len() >= 5 && tokens[0] == "slot" && tokens[4] == "free" {
            *free.entry(tokens[1].to_owned()).or_insert(0) += 1;
        }
    }
    free
}

fn json_err(error: serde_json::Error) -> VivariumError {
    VivariumError::Other(format!("failed to encode JSON: {error}"))
}
