//! Allow and report (operator ruling, 2026-11-04): a hand may change files
//! outside its slice's write scope, and the pump says so. The runner lists
//! the files its published commit changes (a machine fact; the hand's own
//! `change` list is only its report); a path no scope entry covers is posted
//! on the need thread (so the auditor and the operator see it), raised once as
//! `flow-scope-exceeded`, and added to the slice's deviations (so later hands
//! and the slicer see it). Nothing is refused.

use crate::pump::attempt_record;

/// Exception kind: a built slice changed files outside its write scope.
pub const SCOPE_EXCEEDED: &str = "flow-scope-exceeded";

/// The summary line prefix the pool writes for a write job's changed files.
const CHANGED_PREFIX: &str = "changed ";

/// The files a pool receipt says the published commit changed: the exact
/// count and the listed paths (the runner lists at most a fixed number).
/// `None` when the summary has no such line (no commit, or an older runner).
#[must_use]
pub(crate) fn changed(receipt: &str) -> Option<(usize, Vec<String>)> {
    attempt_record::summary_lines(receipt).find_map(|line| {
        let rest = line.trim().strip_prefix(CHANGED_PREFIX)?;
        let (count, paths) = rest.split_once(": ")?;
        let count = count
            .strip_suffix(" paths")
            .or_else(|| count.strip_suffix(" path"))?;
        let count: usize = match count.trim().parse() {
            Ok(count) => count,
            Err(_) => return None,
        };
        let paths = paths
            .split(", ")
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .collect();
        Some((count, paths))
    })
}

/// Whether a write scope entry covers `path`: the same path, or a directory
/// entry (with or without a trailing slash) above it.
#[must_use]
pub(crate) fn covers(entry: &str, path: &str) -> bool {
    crate::pump::flow::scope_policy::covers(entry, path)
}

/// The paths of `changed` that no entry of `scope` covers, in order.
#[must_use]
pub(crate) fn outside(scope: &[String], changed: &[String]) -> Vec<String> {
    changed
        .iter()
        .filter(|path| !scope.iter().any(|entry| covers(entry, path)))
        .cloned()
        .collect()
}

/// The open slices of the run whose scopes hold any of `paths`, each with the
/// paths it holds.
#[must_use]
pub(crate) fn holders(
    open: &[(String, Vec<String>)],
    paths: &[String],
) -> Vec<(String, Vec<String>)> {
    open.iter()
        .filter_map(|(task, scope)| {
            let held: Vec<String> = paths
                .iter()
                .filter(|path| scope.iter().any(|entry| covers(entry, path)))
                .cloned()
                .collect();
            (!held.is_empty()).then(|| (task.clone(), held))
        })
        .collect()
}

/// The one-line report: which slice changed which out-of-scope paths, whether
/// the runner's list was cut short, and which open slices hold any of them.
#[must_use]
pub(crate) fn text(
    node: &str,
    task: &str,
    job: &str,
    out: &[String],
    unlisted: usize,
    held: &[(String, Vec<String>)],
) -> String {
    let more = if unlisted > 0 {
        format!(
            " ({unlisted} more changed path(s) were not listed by the runner, so there may be others)"
        )
    } else {
        String::new()
    };
    let mut line = format!(
        "slice {node} (task {task}) job {job} changed {} path(s) outside its write scope: {}{more}",
        out.len(),
        out.join(", ")
    );
    if !held.is_empty() {
        let names: Vec<String> = held
            .iter()
            .map(|(task, paths)| format!("{task} ({})", paths.join(", ")))
            .collect();
        line.push_str(&format!(
            "; open slice(s) of this run hold some of them, so a merge conflict is likely: {}",
            names.join("; ")
        ));
    }
    line
}

#[cfg(test)]
#[path = "scope_report_test.rs"]
mod tests;
