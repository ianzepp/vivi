//! Repository-relative path rules shared by declared scopes and scope grants.

use vivi_mail::letter::roles::Slice;

/// Whether `entry` covers the same tree path or a descendant directory path.
#[must_use]
pub(crate) fn covers(entry: &str, path: &str) -> bool {
    entry == path
        || (!entry.is_empty()
            && path
                .strip_prefix(entry)
                .is_some_and(|rest| rest.starts_with('/')))
}

/// Reject paths that cannot safely name a repository tree entry.
#[must_use]
pub(crate) fn invalid_path(path: &str) -> Option<&'static str> {
    if path.starts_with('/') {
        return Some("absolute paths are not repository-relative");
    }
    let components: Vec<_> = path.split('/').collect();
    if components.contains(&"..") {
        return Some("path traverses through ..");
    }
    if matches!(components.first(), Some(&".git" | &".vivi")) {
        return Some("path is inside a protected repository root");
    }
    None
}

/// Validate target names and declared write scopes on a slicer batch.
#[must_use]
pub(crate) fn declared_problems(slices: &[Slice], known: &[String]) -> Vec<String> {
    let mut problems = Vec::new();
    for slice in slices {
        if !known.contains(&slice.target) {
            problems.push(format!(
                "slice {} names unknown target {}",
                slice.id, slice.target
            ));
        }
        if slice.write_scope.is_empty() || slice.write_scope.iter().all(|path| path.is_empty()) {
            problems.push(format!("slice {} has an empty write_scope", slice.id));
        }
        for path in &slice.write_scope {
            if let Some(reason) = invalid_path(path) {
                problems.push(format!(
                    "slice {} has invalid write_scope path {path:?}: {reason}",
                    slice.id
                ));
            }
        }
    }
    problems
}

#[cfg(test)]
#[path = "scope_policy_test.rs"]
mod tests;
