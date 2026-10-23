//! `vivi config show | validate | migrate | set | unset`.
#![allow(clippy::missing_errors_doc)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use vivi::VivariumError;
use vivi::cli::ConfigCommand;
use vivi::mailspace::Mailspace;
use vivi::mailspace::config_edit;
use vivi::mailspace::inherit::{self, ConfigOrigins};
use vivi::routing::{
    LegacyCharter, MigrateInputs, MigratePlan, apply_plan_to_document, format_report, overview,
    plan_migrate, render_plan, render_text, require_ok, unified_diff,
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
        ConfigCommand::Set {
            key,
            value,
            project,
        } => set(key, value, project.as_deref()),
        ConfigCommand::Unset { key, project } => unset(key, project.as_deref()),
    }
}

fn set(key: &str, value: &str, project: Option<&Path>) -> Result<(), VivariumError> {
    let root = project_root(project)?;
    let done = config_edit::set_key(&root, key, value)?;
    println!("set {key} = {} ({})", done.value, done.kind);
    for line in done.write.notes() {
        eprintln!("{line}");
    }
    Ok(())
}

fn unset(key: &str, project: Option<&Path>) -> Result<(), VivariumError> {
    let root = project_root(project)?;
    let write = config_edit::unset_key(&root, key)?;
    println!("unset {key}");
    for line in write.notes() {
        eprintln!("{line}");
    }
    Ok(())
}

/// The project root for a writer: `--project`, or the nearest ancestor of the
/// current directory holding a config. The config is not loaded, so a config
/// that does not load can still be repaired through the guard.
fn project_root(project: Option<&Path>) -> Result<PathBuf, VivariumError> {
    if let Some(project) = project {
        return Ok(project.to_path_buf());
    }
    let cwd = std::env::current_dir()?;
    match cwd
        .ancestors()
        .find(|path| path.join(".vivi").join("mailspace.toml").is_file())
    {
        Some(root) => Ok(root.to_path_buf()),
        None => Err(VivariumError::Message(format!(
            "No Vivi mailspace found.\ncwd: {}\ninit: vivi mailspace init",
            cwd.display()
        ))),
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
    let origins = origins_of(&mailspace)?;
    if json {
        let mut value = serde_json::to_value(&view).map_err(json_err)?;
        if let (true, Some(object)) = (origins.inherited(), value.as_object_mut()) {
            object.insert(
                "origins".into(),
                serde_json::to_value(&origins).map_err(json_err)?,
            );
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&value).map_err(json_err)?
        );
        return Ok(());
    }
    if porcelain {
        print_porcelain(&view, &origins);
        return Ok(());
    }
    println!("{}", render_text(&view));
    print!("{}", origins.render());
    Ok(())
}

/// Where the merged config of this mailspace came from.
fn origins_of(mailspace: &Mailspace) -> Result<ConfigOrigins, VivariumError> {
    let path = mailspace.config_path();
    let raw = fs::read_to_string(&path)?;
    Ok(inherit::load_with_origins(&path, &raw)?.origins)
}

fn print_porcelain(view: &vivi::routing::ConfigOverview, origins: &ConfigOrigins) {
    if origins.inherited() {
        print_origins_porcelain(origins);
    }
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
        "flow {} {} {} {} {} {}",
        flow.merge_role,
        flow.merge_pool,
        flow.merge_lanes,
        flow.gate_role,
        flow.gate_pool,
        flow.main
    );
}

/// `file <n> base|project <path>` per merged file, then `origin <section>
/// <n>:<keys> ...` per section.
fn print_origins_porcelain(origins: &ConfigOrigins) {
    for (index, file) in origins.files.iter().enumerate() {
        let tag = if index + 1 == origins.files.len() {
            "project"
        } else {
            "base"
        };
        println!("file {} {tag} {}", index + 1, file.display());
    }
    for entry in &origins.entries {
        let parts: Vec<String> = entry
            .by_file
            .iter()
            .map(|(file, keys)| format!("{}:{}", file + 1, keys.join(",")))
            .collect();
        println!("origin {} {}", entry.section, parts.join(" "));
    }
}

/// Exit status of `vivi config validate` for a project that is valid but not
/// ready. A wrong value exits 1 like any other error; ready exits 0.
const NOT_READY_EXIT: i32 = 3;

fn validate_cmd(json: bool, project: Option<&Path>) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let origins = origins_of(&mailspace)?;
    let mut issues = vivi::pump::readiness::assess(&mailspace);
    issues.extend(missing_rung_issues(&mailspace));
    issues.extend(retired_key_warnings(&mailspace)?);
    issues.extend(
        origins
            .warnings()
            .into_iter()
            .map(|message| vivi::routing::Issue {
                severity: vivi::routing::Severity::Warning,
                message,
                item: None,
            }),
    );
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&issues_json(&issues)).map_err(json_err)?
        );
    } else {
        print_report(&issues);
        print!("{}", origins.render());
    }
    require_ok(&issues)?;
    if !vivi::pump::readiness::is_ready(&issues) {
        std::process::exit(NOT_READY_EXIT);
    }
    Ok(())
}

/// One not-ready item per required rung (`task`, `need`, `main`) a pumped
/// project has not set. Only `config validate` asks: the pump's own readiness
/// does not hold on rungs.
fn missing_rung_issues(mailspace: &Mailspace) -> Vec<vivi::routing::Issue> {
    mailspace
        .config
        .pump
        .iter()
        .flat_map(vivi::pump::config::PumpConfig::missing_rungs)
        .map(|missing| vivi::routing::Issue {
            severity: vivi::routing::Severity::NotReady,
            message: missing.detail,
            item: Some(missing.name),
        })
        .collect()
}

/// One warning per retired key the project file still carries, from the same
/// list `config migrate` removes.
fn retired_key_warnings(mailspace: &Mailspace) -> Result<Vec<vivi::routing::Issue>, VivariumError> {
    let doc = config_edit::load(&mailspace.config_path())?;
    let plan = plan_migrate(&mailspace.config, &doc, &MigrateInputs::default());
    Ok(plan
        .removals
        .into_iter()
        .map(|removal| vivi::routing::Issue {
            severity: vivi::routing::Severity::Warning,
            message: format!("[{}] {}: {}", removal.table, removal.key, removal.reason),
            item: None,
        })
        .collect())
}

fn print_report(issues: &[vivi::routing::Issue]) {
    let others: Vec<_> = issues
        .iter()
        .filter(|issue| issue.severity != vivi::routing::Severity::NotReady)
        .cloned()
        .collect();
    let report = format_report(&others);
    if !report.is_empty() {
        println!("{report}");
    }
    let missing = vivi::pump::readiness::render_missing(issues);
    if !missing.is_empty() {
        println!("{missing}");
    }
}

fn issues_json(issues: &[vivi::routing::Issue]) -> serde_json::Value {
    serde_json::json!(
        issues
            .iter()
            .map(|issue| {
                let mut row = serde_json::json!({
                    "severity": match issue.severity {
                        vivi::routing::Severity::Error => "error",
                        vivi::routing::Severity::Warning => "warning",
                        vivi::routing::Severity::NotReady => "not_ready",
                    },
                    "message": issue.message,
                });
                if let Some(item) = &issue.item {
                    row["item"] = serde_json::json!(item);
                }
                row
            })
            .collect::<Vec<_>>()
    )
}

fn migrate(write: bool, project: Option<&Path>) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let path = mailspace.config_path();
    let before = fs::read_to_string(&path)?;
    let mut doc = config_edit::load(&path)?;
    let inputs = migrate_inputs(&mailspace);
    let plan = plan_migrate(&mailspace.config, &doc, &inputs);
    println!("{}", render_plan(&plan));
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
    write_charters(&plan, project)
}

/// What migrate reads besides the document: every role's charter, and the
/// file `[pump.planner] charter` names.
fn migrate_inputs(mailspace: &Mailspace) -> MigrateInputs {
    let mut charters = BTreeMap::new();
    for role in &mailspace.config.identities {
        if let Ok(text) = mailspace.read_charter(&role.name) {
            charters.insert(role.name.clone(), text);
        }
    }
    MigrateInputs {
        charters,
        legacy_planner_charter: legacy_planner_charter(mailspace),
    }
}

fn legacy_planner_charter(mailspace: &Mailspace) -> Option<LegacyCharter> {
    let written = mailspace.config.pump.as_ref()?.planner.charter.clone();
    if written.trim().is_empty() {
        return None;
    }
    let path = match written.strip_prefix("~/") {
        Some(rest) => dirs::home_dir()?.join(rest),
        None => mailspace.root.join(&written),
    };
    let Ok(text) = fs::read_to_string(path) else {
        return None;
    };
    (!text.trim().is_empty()).then_some(LegacyCharter {
        path: written,
        text,
    })
}

/// Write the charters the plan sets, keeping a copy of any charter file that
/// is already there. The role exists by now: the config was written first.
fn write_charters(plan: &MigratePlan, project: Option<&Path>) -> Result<(), VivariumError> {
    if plan.charters.is_empty() {
        return Ok(());
    }
    let mut mailspace = Mailspace::discover(project)?;
    for write in &plan.charters {
        let file = mailspace
            .dir
            .join("charters")
            .join(format!("{}.md", write.role));
        if file.is_file() {
            let stamp = Utc::now().format("%Y%m%dT%H%M%S");
            let backup = file.with_file_name(format!("{}.md.bak.{stamp}", write.role));
            fs::copy(&file, &backup)?;
            println!("backup {}", backup.display());
        }
        mailspace.set_charter(&write.role, &write.text)?;
        println!(
            "charter set for {} from {} ({} bytes)",
            write.role,
            write.origin,
            write.text.len()
        );
    }
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
