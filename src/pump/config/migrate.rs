//! `vivi config migrate`, as a value. The caller prints. Writes match the CLI.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;

use crate::VivariumError;
use crate::mailspace::Mailspace;
use crate::mailspace::config_edit;
use crate::routing::{
    CharterWrite, LegacyCharter, MigrateInputs, MigratePlan, apply_plan_to_document, plan_migrate,
    render_plan, unified_diff,
};

/// What `vivi config migrate` computed, and what it wrote when asked.
#[derive(Debug, Clone)]
pub struct MigrateReport {
    /// `render_plan` text, without a trailing newline.
    pub plan: String,
    /// Unified diff of `mailspace.toml`.
    pub diff: String,
    /// True when `write` was false. Nothing was written.
    pub dry_run: bool,
    /// Timestamped backup of the config, when `write` was true.
    pub config_backup: Option<PathBuf>,
    /// Config path written, when `write` was true.
    pub wrote: Option<PathBuf>,
    /// Charters written, in plan order.
    pub charters: Vec<WrittenCharter>,
    /// The plan itself.
    pub planned: MigratePlan,
}

/// One charter file migrate wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenCharter {
    pub role: String,
    pub origin: String,
    /// `text.len()` of the charter that was written.
    pub bytes: usize,
    /// Backup of the previous charter file, when one existed.
    pub backup: Option<PathBuf>,
}

/// Plan a migration of `project`, and apply it when `write` is true.
///
/// A write keeps a timestamped `.bak` of the config, then writes charters
/// from the plan (backing up any charter file already there). This function
/// does not print.
///
/// # Errors
/// Returns an error when the mailspace cannot be read, the plan cannot be
/// applied, or a write fails. A failed charter write may leave the config
/// and earlier charters already updated, matching the CLI.
pub fn migrate(project: Option<&Path>, write: bool) -> Result<MigrateReport, VivariumError> {
    let prepared = prepare(project)?;
    if !write {
        return Ok(finish(prepared, true, None, None, Vec::new()));
    }
    let backup = write_backup(&prepared.path, &prepared.before)?;
    config_edit::write_text(&prepared.path, &prepared.after)?;
    let config_path = prepared.path.clone();
    let charters = write_charters(&prepared.plan, project)?;
    Ok(finish(
        prepared,
        false,
        Some(backup),
        Some(config_path),
        charters,
    ))
}

struct Prepared {
    path: PathBuf,
    before: String,
    after: String,
    plan_text: String,
    diff: String,
    plan: MigratePlan,
}

fn prepare(project: Option<&Path>) -> Result<Prepared, VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let path = mailspace.config_path();
    let before = fs::read_to_string(&path)?;
    let mut doc = config_edit::load(&path)?;
    let inputs = migrate_inputs(&mailspace);
    let plan = plan_migrate(&mailspace.config, &doc, &inputs);
    let plan_text = render_plan(&plan);
    apply_plan_to_document(&mut doc, &plan)?;
    let after = doc.to_string();
    let diff = unified_diff(&before, &after, "mailspace.toml");
    Ok(Prepared {
        path,
        before,
        after,
        plan_text,
        diff,
        plan,
    })
}

fn finish(
    prepared: Prepared,
    dry_run: bool,
    config_backup: Option<PathBuf>,
    wrote: Option<PathBuf>,
    charters: Vec<WrittenCharter>,
) -> MigrateReport {
    MigrateReport {
        plan: prepared.plan_text,
        diff: prepared.diff,
        dry_run,
        config_backup,
        wrote,
        charters,
        planned: prepared.plan,
    }
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
    let path = planner_charter_path(mailspace, &written)?;
    let Ok(text) = fs::read_to_string(path) else {
        return None;
    };
    (!text.trim().is_empty()).then_some(LegacyCharter {
        path: written,
        text,
    })
}

fn planner_charter_path(mailspace: &Mailspace, written: &str) -> Option<PathBuf> {
    match written.strip_prefix("~/") {
        Some(rest) => Some(dirs::home_dir()?.join(rest)),
        None => Some(mailspace.root.join(written)),
    }
}

/// Write the charters the plan sets, keeping a copy of any charter file that
/// is already there. The role exists by now: the config was written first.
fn write_charters(
    plan: &MigratePlan,
    project: Option<&Path>,
) -> Result<Vec<WrittenCharter>, VivariumError> {
    if plan.charters.is_empty() {
        return Ok(Vec::new());
    }
    let mut mailspace = Mailspace::discover(project)?;
    let mut written = Vec::with_capacity(plan.charters.len());
    for write in &plan.charters {
        written.push(write_one_charter(&mut mailspace, write)?);
    }
    Ok(written)
}

fn write_one_charter(
    mailspace: &mut Mailspace,
    write: &CharterWrite,
) -> Result<WrittenCharter, VivariumError> {
    let file = mailspace
        .dir
        .join("charters")
        .join(format!("{}.md", write.role));
    let backup = backup_charter(&file, &write.role)?;
    mailspace.set_charter(&write.role, &write.text)?;
    Ok(WrittenCharter {
        role: write.role.clone(),
        origin: write.origin.clone(),
        bytes: write.text.len(),
        backup,
    })
}

fn backup_charter(file: &Path, role: &str) -> Result<Option<PathBuf>, VivariumError> {
    if !file.is_file() {
        return Ok(None);
    }
    let stamp = Utc::now().format("%Y%m%dT%H%M%S");
    let backup = file.with_file_name(format!("{role}.md.bak.{stamp}"));
    fs::copy(file, &backup)?;
    Ok(Some(backup))
}

fn write_backup(path: &Path, before: &str) -> Result<PathBuf, VivariumError> {
    let stamp = Utc::now().format("%Y%m%dT%H%M%S");
    let name = path
        .file_name()
        .and_then(|os| os.to_str())
        .unwrap_or("mailspace.toml");
    let backup = path.with_file_name(format!("{name}.bak.{stamp}"));
    fs::write(&backup, before)?;
    Ok(backup)
}

#[cfg(test)]
#[path = "migrate_test.rs"]
mod tests;
