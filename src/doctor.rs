//! The top-level, read-only `vivi doctor` report model.

use serde::Serialize;

pub use crate::pool::doctor::Status;

pub mod disk_section;
pub mod install_section;
pub mod mailspace_section;
pub mod pools_section;
pub mod proxy_section;
pub mod pump_section;
pub mod roles_section;
pub mod sync_section;

/// The overall health of a doctor report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Ok,
    Warn,
    Fail,
}

impl Verdict {
    /// The verdict as a lowercase word.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }

    /// Return whichever verdict is more severe.
    #[must_use]
    pub const fn worst(self, other: Self) -> Self {
        match (self, other) {
            (Self::Fail, _) | (_, Self::Fail) => Self::Fail,
            (Self::Warn, _) | (_, Self::Warn) => Self::Warn,
            (Self::Ok, Self::Ok) => Self::Ok,
        }
    }
}

/// One finding in a top-level doctor section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub section: String,
    pub name: String,
    pub status: Status,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

impl Finding {
    fn new(section: &str, name: &str, status: Status, detail: &str, fix: Option<String>) -> Self {
        Self {
            section: section.to_string(),
            name: name.to_string(),
            status,
            detail: detail.to_string(),
            fix,
        }
    }

    #[must_use]
    pub fn ok(section: &str, name: &str, detail: &str) -> Self {
        Self::new(section, name, Status::Ok, detail, None)
    }

    #[must_use]
    pub fn warn(
        section: &str,
        name: &str,
        detail: &str,
        fix: impl Into<String>,
    ) -> Self {
        Self::new(section, name, Status::Warn, detail, Some(fix.into()))
    }

    #[must_use]
    pub fn fail(
        section: &str,
        name: &str,
        detail: &str,
        fix: impl Into<String>,
    ) -> Self {
        Self::new(section, name, Status::Fail, detail, Some(fix.into()))
    }

    #[must_use]
    pub fn skipped(section: &str, name: &str, reason: &str) -> Self {
        Self::new(section, name, Status::Skipped, reason, None)
    }
}

/// The overall verdict and ranked findings from `vivi doctor`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorReport {
    pub verdict: Verdict,
    pub findings: Vec<Finding>,
}

impl DoctorReport {
    /// Build a report, ignoring skipped checks when calculating its verdict.
    #[must_use]
    pub fn from_findings(mut findings: Vec<Finding>) -> Self {
        let verdict = findings.iter().fold(Verdict::Ok, |current, finding| {
            match finding.status {
                Status::Fail => current.worst(Verdict::Fail),
                Status::Warn => current.worst(Verdict::Warn),
                Status::Ok | Status::Skipped => current,
            }
        });
        findings.sort_by_key(|finding| match finding.status {
            Status::Fail => 0,
            Status::Warn => 1,
            Status::Ok => 2,
            Status::Skipped => 3,
        });
        Self { verdict, findings }
    }
}

#[cfg(test)]
#[path = "doctor_test.rs"]
mod tests;
