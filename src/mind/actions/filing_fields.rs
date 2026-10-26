//! Filing content checks for [`Action::FileNeed`] and
//! [`Action::FileFixNeed`]: those actions carry prose plus typed sugar,
//! not a complete front-matter letter, so subject, body, done-when
//! clauses and the optional write scope are bounded here, and the
//! vivi-only fix policy lives here too.

use std::ffi::OsStr;
use std::path::{Component, Path};

use super::{ActionRequest, Refusal};

/// Longest subject a filing may carry.
pub(super) const MAX_SUBJECT: usize = 200;
/// Longest body a filing may carry.
pub(super) const MAX_BODY: usize = 16_000;
/// Longest single done-when clause.
pub(super) const MAX_CLAUSE: usize = 500;
/// Most done-when clauses one filing may carry.
pub(super) const MAX_CLAUSES: usize = 8;
/// Longest single write-scope path.
pub(super) const MAX_WRITE_PATH: usize = 256;
/// Most paths one filing's write scope may name.
pub(super) const MAX_WRITE_PATHS: usize = 32;
/// The only project file-fix-need may target.
const FIX_PROJECT: &str = "vivi";

/// Validate the filing content: bounded subject, body and done-when
/// clauses, and an optional bounded write scope. Oversized content is
/// [`Refusal::FilingTooLarge`] naming the field; malformed content —
/// front-matter bodies, blank clauses, escaping paths — is
/// [`Refusal::InvalidField`]. Absence is the field matrix's business and
/// passes here.
///
/// # Errors
/// Returns the first offending field.
pub(super) fn validate_filing(request: &ActionRequest) -> Result<(), Refusal> {
    check_subject(request.subject.as_deref())?;
    check_body(request.body.as_deref())?;
    check_clauses(&request.done_when)?;
    check_write_scope(&request.write_scope)
}

fn check_subject(subject: Option<&str>) -> Result<(), Refusal> {
    if subject.is_some_and(|subject| subject.chars().count() > MAX_SUBJECT) {
        return Err(Refusal::FilingTooLarge("subject"));
    }
    Ok(())
}

fn check_body(body: Option<&str>) -> Result<(), Refusal> {
    let Some(body) = body else { return Ok(()) };
    if body.trim_start().starts_with("+++") {
        return Err(Refusal::InvalidField("body"));
    }
    if body.chars().count() > MAX_BODY {
        return Err(Refusal::FilingTooLarge("body"));
    }
    Ok(())
}

fn check_clauses(clauses: &[String]) -> Result<(), Refusal> {
    if clauses.iter().any(|clause| clause.trim().is_empty()) {
        return Err(Refusal::InvalidField("done_when"));
    }
    if clauses.len() > MAX_CLAUSES
        || clauses
            .iter()
            .any(|clause| clause.chars().count() > MAX_CLAUSE)
    {
        return Err(Refusal::FilingTooLarge("done_when"));
    }
    Ok(())
}

fn check_write_scope(paths: &[String]) -> Result<(), Refusal> {
    if paths.iter().any(|path| !is_scoped_path(path)) {
        return Err(Refusal::InvalidField("write_scope"));
    }
    if paths.len() > MAX_WRITE_PATHS
        || paths
            .iter()
            .any(|path| path.chars().count() > MAX_WRITE_PATH)
    {
        return Err(Refusal::FilingTooLarge("write_scope"));
    }
    Ok(())
}

/// A write-scope path is nonblank, relative, and climbs no higher than
/// the project root.
fn is_scoped_path(path: &str) -> bool {
    let as_path = Path::new(path);
    !path.trim().is_empty()
        && !as_path.is_absolute()
        && !as_path
            .components()
            .any(|component| component == Component::ParentDir)
}

/// The fix policy: file-fix-need fixes this codebase, so the watched
/// project root must itself be named `vivi` and the target must say so.
/// General file-need may file to any watched project. The caller has
/// already checked the project is watched.
///
/// # Errors
/// Returns [`Refusal::InvalidField`] naming the offending field.
pub(super) fn check_fix_policy(request: &ActionRequest, project: &Path) -> Result<(), Refusal> {
    if project.file_name() != Some(OsStr::new(FIX_PROJECT)) {
        return Err(Refusal::InvalidField("project"));
    }
    if request.target.as_deref() != Some(FIX_PROJECT) {
        return Err(Refusal::InvalidField("target"));
    }
    Ok(())
}
