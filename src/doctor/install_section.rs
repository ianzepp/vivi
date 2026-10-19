//! The `install` section of `vivi doctor`: binary version, release standing,
//! and the health of recorded skill copies.

use std::path::{Path, PathBuf};

use super::Finding;
use crate::skills::SkillCopyStatus;
use crate::update::CheckOutcome;

/// Report the running binary version, its standing against the newest public
/// release, and one finding per recorded skill-copy directory.
///
/// Read-only and offline-tolerant: a release check that was not run, or one
/// that failed, only skips its finding — it never fails the section.
#[must_use]
pub fn findings(
    binary_version: &str,
    release: Option<Result<&CheckOutcome, String>>,
    targets: &[(PathBuf, SkillCopyStatus)],
) -> Vec<Finding> {
    let mut out = vec![Finding::ok(
        "install",
        "binary",
        &format!("vivi {binary_version}"),
    )];
    out.push(release_finding(release));
    out.extend(skills_findings(targets));
    out
}

fn release_finding(release: Option<Result<&CheckOutcome, String>>) -> Finding {
    match release {
        None => Finding::skipped("release", "release", "release check not run"),
        Some(Ok(CheckOutcome::Available)) => {
            Finding::warn("release", "release", "a newer public release exists", "vivi update")
        }
        Some(Ok(CheckOutcome::UpToDate)) => {
            Finding::ok("release", "release", "this binary is the newest public release")
        }
        Some(Ok(CheckOutcome::Ahead)) => {
            Finding::ok("release", "release", "this binary is newer than the newest public release")
        }
        Some(Err(reason)) => {
            Finding::skipped("release", "release", &format!("release check unavailable: {reason}"))
        }
    }
}

fn skills_findings(targets: &[(PathBuf, SkillCopyStatus)]) -> Vec<Finding> {
    if targets.is_empty() {
        return vec![Finding::skipped("skills", "skills", "no installed skills recorded")];
    }
    targets.iter().map(|(dir, status)| copy_finding(dir, status)).collect()
}

fn copy_finding(dir: &Path, status: &SkillCopyStatus) -> Finding {
    let shown = dir.display().to_string();
    match status {
        SkillCopyStatus::UpToDate => Finding::ok("skills", "skills", &format!("{shown}: up to date")),
        SkillCopyStatus::Stale(names) => Finding::warn(
            "skills",
            "skills",
            &format!("{shown}: stale copies: {}", names.join(", ")),
            format!("vivi skills install {shown} --force"),
        ),
        SkillCopyStatus::NotInstalled => {
            Finding::skipped("skills", "skills", &format!("{shown}: no catalog skill present"))
        }
    }
}

#[cfg(test)]
#[path = "install_section_test.rs"]
mod tests;
