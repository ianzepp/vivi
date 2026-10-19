//! The top-level, read-only `vivi doctor` report model.

use std::path::{Path, PathBuf};

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

/// Parsed project data for doctor checks. This reader never creates the
/// mailspace id, `.gitignore`, database, or any missing directory.
#[derive(Debug, Clone)]
pub struct Project {
    pub root: PathBuf,
    pub dir: PathBuf,
    pub config: crate::mailspace::MailspaceConfig,
}

impl Project {
    /// Find and parse the project mailspace without invoking write-capable
    /// `Mailspace::discover`.
    pub fn discover(explicit: Option<&Path>) -> Result<Self, String> {
        let root = match explicit {
            Some(path) => path.to_path_buf(),
            None => std::env::current_dir()
                .map_err(|error| format!("could not read current directory: {error}"))?
                .ancestors()
                .find(|path| path.join(".vivi/mailspace.toml").is_file())
                .map(Path::to_path_buf)
                .ok_or_else(|| {
                    "no .vivi/mailspace.toml found in the current directory or its parents"
                        .to_string()
                })?,
        };
        let dir = root.join(".vivi");
        let path = dir.join("mailspace.toml");
        let raw = std::fs::read_to_string(&path)
            .map_err(|error| format!("cannot read mailspace config: {error}"))?;
        let config = toml::from_str(&raw)
            .map_err(|error| format!("mailspace config is invalid: {error}"))?;
        Ok(Self { root, dir, config })
    }

    /// Open the existing mail database read-only, without schema migration.
    pub fn open_storage_read_only(&self) -> Result<crate::storage::Storage, String> {
        crate::storage::Storage::open_mailspace_read_only(&self.dir)
            .map_err(|error| format!("mail store cannot be opened read-only: {error}"))
    }
}

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
    pub fn warn(section: &str, name: &str, detail: &str, fix: impl Into<String>) -> Self {
        Self::new(section, name, Status::Warn, detail, Some(fix.into()))
    }

    #[must_use]
    pub fn fail(section: &str, name: &str, detail: &str, fix: impl Into<String>) -> Self {
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
        let verdict = findings
            .iter()
            .fold(Verdict::Ok, |current, finding| match finding.status {
                Status::Fail => current.worst(Verdict::Fail),
                Status::Warn => current.worst(Verdict::Warn),
                Status::Ok | Status::Skipped => current,
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
