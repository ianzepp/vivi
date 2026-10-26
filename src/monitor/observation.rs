//! Shared read-health vocabulary for the monitor: what one project's
//! observation contains, which read problems were seen, and which expected
//! databases were actually present.
//!
//! Classification is explicit: an observation is complete only when
//! [`ProjectObservation::issues`] is empty. Detail strings are display text,
//! never an input to classification.

use std::path::PathBuf;

use super::model::MonitorRow;

/// Which subsystem a read problem or expected store belongs to.
///
/// Serde shape: serializes as `project-config`, `mail-store`, `pool-state`,
/// or `pump-state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum IssueKind {
    ProjectConfig,
    MailStore,
    PoolState,
    PumpState,
}

/// One problem seen while reading a project's mailspace.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReadIssue {
    pub kind: IssueKind,
    /// Display text for humans; never parsed back into a classification.
    pub detail: String,
}

/// An expected mail/pump database and whether it was actually present on
/// disk. A store that is no longer configured is omitted entirely, rather
/// than reported missing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StorePresence {
    pub path: PathBuf,
    pub kind: IssueKind,
    pub present: bool,
}

/// One full read of a project: its rows, the issues hit while reading, and
/// the presence of its expected stores.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct ProjectObservation {
    pub rows: Vec<MonitorRow>,
    pub issues: Vec<ReadIssue>,
    pub stores: Vec<StorePresence>,
}

impl ProjectObservation {
    /// True when the read saw no problems at all.
    pub fn is_complete(&self) -> bool {
        self.issues.is_empty()
    }

    /// True when any read problem was recorded.
    pub fn is_degraded(&self) -> bool {
        !self.issues.is_empty()
    }
}

/// An active/outage summary for one project root: one entry per root, not
/// per display name.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProjectWarning {
    pub root: PathBuf,
    pub issues: Vec<ReadIssue>,
}

#[cfg(test)]
#[path = "observation_test.rs"]
mod tests;
