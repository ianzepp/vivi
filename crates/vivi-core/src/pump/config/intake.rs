//! Target-local intake allowlists and branch-name validation.

use std::collections::BTreeSet;

use super::PumpTarget;

const INTAKE_RUNGS: [&str; 3] = ["task", "need", "main"];
const OWNED_NAMESPACES: [&str; 2] = ["vivi/", "pump/"];

/// Validate intake branches and their minimum rung for one target.
pub(super) fn validate_target_intake(id: &str, target: &PumpTarget) -> Vec<String> {
    let prefix = format!("[pump.targets.{id}]");
    let mut errors = Vec::new();
    if !INTAKE_RUNGS.contains(&target.intake_rung.as_str()) {
        errors.push(format!(
            "{prefix} intake_rung must be one of task, need or main"
        ));
    }

    let mut seen = BTreeSet::new();
    for branch in &target.intake {
        if !seen.insert(branch) {
            errors.push(format!("{prefix} intake has duplicate branch `{branch}`"));
        }
        if !valid_branch_name(branch) {
            errors.push(format!(
                "{prefix} invalid intake branch `{branch}`: expected a literal branch name"
            ));
        } else if branch == &target.main {
            errors.push(format!(
                "{prefix} intake branch `{branch}` is the target main branch"
            ));
        } else if OWNED_NAMESPACES
            .iter()
            .any(|namespace| branch.starts_with(namespace))
        {
            errors.push(format!(
                "{prefix} intake branch `{branch}` is in a pump-owned working namespace"
            ));
        }
    }
    errors
}

/// Whether `branch` is a safe, unambiguous short name below `refs/heads/`.
fn valid_branch_name(branch: &str) -> bool {
    if branch.is_empty()
        || branch == "@"
        || branch.starts_with("refs/")
        || branch.starts_with('-')
        || branch.starts_with('+')
        || branch.starts_with('/')
        || branch.ends_with('/')
        || branch.ends_with('.')
        || branch.contains("//")
        || branch.contains("..")
        || branch.contains("@{")
    {
        return false;
    }
    if branch.bytes().any(|byte| {
        byte <= b' '
            || byte == 0x7f
            || matches!(byte, b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b']' | b'\\')
    }) {
        return false;
    }
    branch.split('/').all(|component| {
        !component.is_empty() && !component.starts_with('.') && !component.ends_with(".lock")
    })
}

#[cfg(test)]
#[path = "intake_test.rs"]
mod tests;
