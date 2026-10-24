//! `vivi config show | validate | migrate | set | unset`.
#![allow(clippy::missing_errors_doc)]

use std::path::{Path, PathBuf};

use vivi::VivariumError;
use vivi::cli::ConfigCommand;
use vivi::mailspace::config_edit;
use vivi::mailspace::inherit::ConfigOrigins;
use vivi::pump::config::{self, MigrateReport};
use vivi::routing::{ConfigOverview, format_report, render_text, require_ok};

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
    let report = config::show(project, resolved)?;
    let view = &report.overview;
    let origins = &report.origins;
    if json {
        let mut value = serde_json::to_value(view).map_err(json_err)?;
        if let (true, Some(object)) = (origins.inherited(), value.as_object_mut()) {
            object.insert(
                "origins".into(),
                serde_json::to_value(origins).map_err(json_err)?,
            );
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&value).map_err(json_err)?
        );
        return Ok(());
    }
    if porcelain {
        print_porcelain(view, origins);
        return Ok(());
    }
    println!("{}", render_text(view));
    print!("{}", origins.render());
    Ok(())
}

fn print_porcelain(view: &ConfigOverview, origins: &ConfigOrigins) {
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
        "flow {} {} {} {}",
        flow.merge_role, flow.merge_pool, flow.merge_lanes, flow.main
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
    let report = config::validate(project)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&config::issues_json(&report.issues)).map_err(json_err)?
        );
    } else {
        print_report(&report.issues);
        print!("{}", report.origins.render());
    }
    require_ok(&report.issues)?;
    if !vivi::pump::readiness::is_ready(&report.issues) {
        std::process::exit(NOT_READY_EXIT);
    }
    Ok(())
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

fn migrate(write: bool, project: Option<&Path>) -> Result<(), VivariumError> {
    let report = config::migrate(project, write)?;
    print_migrate(&report);
    Ok(())
}

fn print_migrate(report: &MigrateReport) {
    println!("{}", report.plan);
    print!("{}", report.diff);
    if report.dry_run {
        println!("dry-run; pass --write to apply (a timestamped .bak is kept)");
        return;
    }
    if let Some(backup) = &report.config_backup {
        println!("backup {}", backup.display());
    }
    if let Some(path) = &report.wrote {
        println!("wrote {} (backup beside it)", path.display());
    }
    for charter in &report.charters {
        if let Some(backup) = &charter.backup {
            println!("backup {}", backup.display());
        }
        println!(
            "charter set for {} from {} ({} bytes)",
            charter.role, charter.origin, charter.bytes
        );
    }
}

fn json_err(error: serde_json::Error) -> VivariumError {
    VivariumError::Other(format!("failed to encode JSON: {error}"))
}
