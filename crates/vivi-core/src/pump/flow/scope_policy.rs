//! Repository-relative path policy shared by declared scopes and grants.

use std::path::{Component, Path};
use vivi_mail::letter::roles::Slice;

/// Whether `path` is a safe repository-relative tree path.
#[must_use]
pub(crate) fn valid_path(path: &str) -> bool {
    if path.is_empty() || Path::new(path).is_absolute() {
        return false;
    }
    let mut any = false;
    for component in Path::new(path).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return false,
            Component::Normal(part) => {
                any = true;
                if part == ".git" || part == ".vivi" {
                    return false;
                }
            }
        }
    }
    any
}

/// Component-boundary coverage, preserving all pathname whitespace.
#[must_use]
pub(crate) fn covers(entry: &str, path: &str) -> bool {
    let entry = entry.trim_end_matches('/');
    entry == path
        || path
            .strip_prefix(entry)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Declared-slice checks shared by initial fanout and reslice validation.
pub(crate) fn declared_problems(slices: &[Slice], known: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for slice in slices {
        if !known.contains(&slice.target) {
            out.push(format!(
                "slice {} names unknown target {}",
                slice.id, slice.target
            ));
        }
        if slice.write_scope.is_empty() {
            out.push(format!("slice {} has an empty write_scope", slice.id));
        }
        for path in &slice.write_scope {
            if !valid_path(path) {
                out.push(format!(
                    "slice {} has invalid repository path {path:?}",
                    slice.id
                ));
            }
        }
    }
    out
}

#[cfg(test)]
#[path = "scope_policy_test.rs"]
mod tests;
