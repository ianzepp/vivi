//! The shared GitHub `source` validator for the hosted creation tools
//! (`create_project` and `lane_create`). Both accept a repository to fill
//! the new target from; this module is the one policy for what they admit:
//! only `https://github.com/<owner>/<repo>` URLs, `.git` optional. Local
//! vivi CLI imports keep their own, wider rules — this lives only here.

use super::super::tools::ToolError;

/// The admitted `source` form, stated as a regex (enforced by hand below,
/// without a regex dependency).
const RULE: &str = r"^https://github\.com/[A-Za-z0-9._-]+/[A-Za-z0-9._-]+(\.git)?$";

/// Check a `source` against the GitHub rule. No trimming, no `.git`
/// stripping, no path normalisation, and no GitHub naming restrictions
/// beyond the rule: the segments are ASCII alphanumeric plus dot, underscore
/// and hyphen.
pub(super) fn check(source: &str) -> Result<(), ToolError> {
    if is_github_url(source) {
        Ok(())
    } else {
        Err(ToolError::new(format!(
            "source must match {RULE} (an https://github.com/<owner>/<repo> URL, .git \
             optional); local paths, file://, ssh://, git@host:owner/repo, other hosts, user \
             info, query and fragment are refused"
        )))
    }
}

/// True when `source` is exactly the rule: the literal prefix, then exactly
/// two slash-separated segments of allowed characters. A query or fragment
/// fails the character test; a trailing slash or extra segment fails the
/// two-segment test; other hosts, schemes and user info fail the prefix.
fn is_github_url(source: &str) -> bool {
    let Some(rest) = source.strip_prefix("https://github.com/") else {
        return false;
    };
    let mut segments = rest.split('/');
    let (Some(owner), Some(repo), None) = (segments.next(), segments.next(), segments.next())
    else {
        return false;
    };
    is_segment(owner) && is_segment(repo)
}

/// Nonempty, ASCII alphanumeric or dot, underscore and hyphen only.
fn is_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

#[cfg(test)]
#[path = "source_test.rs"]
mod tests;
