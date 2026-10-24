//! `vivi config validate`, as a value. The caller prints and chooses the exit code.

use std::path::Path;

use crate::VivariumError;
use crate::mailspace::Mailspace;
use crate::mailspace::config_edit;
use crate::mailspace::inherit::ConfigOrigins;
use crate::pump::readiness;
use crate::routing::{Issue, MigrateInputs, Severity, plan_migrate};

use super::PumpConfig;
use super::origins::origins_of;

/// Findings `vivi config validate` reports, plus where merged values came from.
#[derive(Debug, Clone)]
pub struct ValidateReport {
    pub issues: Vec<Issue>,
    pub origins: ConfigOrigins,
}

/// Assess `project` the way `vivi config validate` does. Does not print or exit.
///
/// Error-severity issues are included in [`ValidateReport::issues`]. They are
/// not returned as `Err`: the caller decides whether they fail the command.
///
/// # Errors
/// Returns an error when the mailspace or its config file cannot be read.
pub fn validate(project: Option<&Path>) -> Result<ValidateReport, VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let origins = origins_of(&mailspace)?;
    let mut issues = readiness::assess(&mailspace);
    issues.extend(missing_rung_issues(&mailspace));
    issues.extend(retired_key_issues(&mailspace)?);
    issues.extend(origin_warnings(&origins));
    Ok(ValidateReport { issues, origins })
}

/// The JSON array `vivi config validate --json` prints.
#[must_use]
pub fn issues_json(issues: &[Issue]) -> serde_json::Value {
    serde_json::json!(
        issues
            .iter()
            .map(|issue| {
                let mut row = serde_json::json!({
                    "severity": match issue.severity {
                        Severity::Error => "error",
                        Severity::Warning => "warning",
                        Severity::NotReady => "not_ready",
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

fn origin_warnings(origins: &ConfigOrigins) -> impl Iterator<Item = Issue> + '_ {
    origins.warnings().into_iter().map(Issue::warning)
}

/// One not-ready item per required rung (`task`, `need`, `main`) a pumped
/// project has not set. Only `config validate` asks: the pump's own readiness
/// does not hold on rungs.
fn missing_rung_issues(mailspace: &Mailspace) -> Vec<Issue> {
    mailspace
        .config
        .pump
        .iter()
        .flat_map(PumpConfig::missing_rungs)
        .map(|missing| Issue::not_ready(missing.name, missing.detail))
        .collect()
}

/// One error per key the project file carries that vivi no longer reads, from
/// the same list `config migrate` removes. The message says how to fix it.
fn retired_key_issues(mailspace: &Mailspace) -> Result<Vec<Issue>, VivariumError> {
    let doc = config_edit::load(&mailspace.config_path())?;
    let plan = plan_migrate(&mailspace.config, &doc, &MigrateInputs::default());
    Ok(plan
        .removals
        .into_iter()
        .map(|removal| {
            Issue::error(format!(
                "[{}] {}: {}; fix: run `vivi config migrate --write` (it removes the key) or \
                 delete the key",
                removal.table, removal.key, removal.reason
            ))
        })
        .collect())
}

#[cfg(test)]
#[path = "check_test.rs"]
mod tests;
