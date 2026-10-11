//! Per-section caps and the truncation manifest.
//!
//! A boot digest is a frame, so it has a budget. Every section caps its rows,
//! records what it dropped, and the digest closes by naming each cap. The
//! reader then knows the digest is complete in coverage and bounded in length,
//! and knows where to look for the rest.
//!
//! Caps are constants rather than flags: boot is one command with one shape,
//! and a caller-tuned budget would make two boot runs incomparable.

use serde::Serialize;

pub const CAP_SEATS: usize = 24;
pub const CAP_LOOPS: usize = 10;
pub const CAP_UNABSORBED: usize = 12;
pub const CAP_UNKNOWN_ROLES: usize = 12;
pub const CAP_TASKS: usize = 20;
pub const CAP_NEEDS: usize = 40;
pub const CAP_WANTS: usize = 20;
pub const CAP_GOALS: usize = 12;
pub const CAP_FRONTIER: usize = 8;
pub const CAP_MEMOS: usize = 10;
pub const CAP_RULES: usize = 10;
pub const CAP_CHARTERS: usize = 8;
pub const CAP_CHARTER_LINES: usize = 6;
pub const CAP_PROBE_LINES: usize = 60;
pub const CAP_TRIAGE_SUBJECTS: usize = 3;
/// Handles per triage slice: small enough to seat one role per slice.
pub const TRIAGE_SLICE: usize = 12;

/// One cap that bit. Names the section, what was kept, and how much was total.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Truncation {
    pub section: String,
    pub shown: usize,
    pub total: usize,
}

impl Truncation {
    /// Record a cap only when it actually dropped rows.
    #[must_use]
    pub fn of(section: &str, shown: usize, total: usize) -> Option<Self> {
        (total > shown).then(|| Self {
            section: section.to_string(),
            shown,
            total,
        })
    }
}

/// Keep the first `cap` rows, reporting the drop when one happened.
pub fn capped<T>(rows: Vec<T>, cap: usize, section: &str, dropped: &mut Vec<Truncation>) -> Vec<T> {
    let total = rows.len();
    if total <= cap {
        return rows;
    }
    record(dropped, section, cap, total);
    rows.into_iter().take(cap).collect()
}

/// Record a cap that a section applied itself.
pub fn record(dropped: &mut Vec<Truncation>, section: &str, shown: usize, total: usize) {
    if let Some(entry) = Truncation::of(section, shown, total) {
        dropped.push(entry);
    }
}
