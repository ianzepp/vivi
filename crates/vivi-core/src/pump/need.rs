//! Branch-name helpers for the unit and quarantine namespaces.
//!
//! The old need-branch lifecycle (freeze, landing, recovery) is gone; flows own
//! the need branch now. What stays is the naming of unit branches and the
//! namespace prefixes that disk, requeue and doctor code match against.

/// Generate a new per-attempt unit branch.
#[must_use]
pub fn unit_branch(uid: &str, attempt: u32) -> String {
    format!("{}{uid}-a{attempt}", unit_namespace_prefix())
}

/// Root prefix for all newly generated attempt branches.
#[must_use]
pub const fn unit_namespace_prefix() -> &'static str {
    "vivi/unit/"
}

/// Root prefix for newly generated quarantine branches.
#[must_use]
pub const fn quarantine_namespace_prefix() -> &'static str {
    "vivi/quarantine/"
}

/// Whether `sha` is a full Git object ID.
#[must_use]
pub fn is_full_sha(sha: &str) -> bool {
    matches!(sha.len(), 40 | 64) && sha.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
#[path = "need_test.rs"]
mod tests;
