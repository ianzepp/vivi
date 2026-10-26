//! Kind defaults for read-only and band detection.

use crate::pool::bands::parse_band;

/// Kinds that write by default (the others that the pump cares about are
/// read-only). Compared case-insensitively.
pub const WRITE_KINDS: [&str; 3] = ["hand", "merge", "lint"];

/// Kinds that default to read-only. Compared case-insensitively.
const READ_ONLY_KINDS: [&str; 10] = [
    "auditor",
    "triage",
    "cadence",
    "steward",
    "head",
    "head-cto",
    "digest",
    "researcher",
    "admitter",
    "slicer",
];

/// Whether a role of `kind` is read-only unless the role sets `read_only`.
#[must_use]
pub fn default_read_only(kind: Option<&str>) -> bool {
    kind.is_some_and(|kind| {
        READ_ONLY_KINDS
            .iter()
            .any(|known| known.eq_ignore_ascii_case(kind))
    })
}

/// Whether `model` is a band string (`P0`–`P5`, optionally `-S0`–`-S3`).
#[must_use]
pub fn is_band_model(model: &str) -> bool {
    parse_band(model.trim()).is_some()
}

/// The resolved read-only flag: the CLI flag, else the role's explicit
/// `read_only`, else the kind default.
#[must_use]
pub fn resolve_read_only(flag: bool, role_flag: Option<bool>, kind: Option<&str>) -> bool {
    if flag {
        return true;
    }
    role_flag.unwrap_or_else(|| default_read_only(kind))
}

#[cfg(test)]
#[path = "defaults_test.rs"]
mod tests;
