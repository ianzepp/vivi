//! Operator unlock: `vivi pump unlock`.
//!
//! Posts an unlock action on the need's open pump record
//! ([`crate::mailspace::PumpActionRequest::unlock`]). The flow applies it at
//! its next tick ([`crate::pump::flow::unlock`]). [`unlock`] does not print;
//! the CLI prints [`UnlockResult`].

use std::fmt;
use std::path::Path;

use crate::VivariumError;
use crate::mailspace::{Mailspace, PumpActionRequest};

/// What [`unlock`] posted. Display is the line the CLI prints.
///
/// `need` is the handle the caller named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnlockResult {
    pub need: String,
    /// The open pump record the action was posted on.
    pub record: String,
    /// The action letter's content id.
    pub id: String,
    /// Entry stored on the action. `None` when the caller named none.
    pub entry: Option<String>,
}

impl fmt::Display for UnlockResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unlock posted on {} for need {}; the pump applies it at its next tick",
            self.record, self.need
        )
    }
}

/// Post an unlock action on `need`'s open pump record.
///
/// `entry` is the flow entry to restart at (the flow's first entry when
/// `None`). `note` is the letter body (`None` posts an empty body). Nothing
/// is printed.
///
/// # Errors
/// Returns an error when the need has no open pump record, or the post fails.
pub fn unlock(
    project: &Path,
    need: &str,
    entry: Option<&str>,
    note: Option<&str>,
) -> Result<UnlockResult, VivariumError> {
    let mailspace = Mailspace::discover(Some(project))?;
    let request = PumpActionRequest::unlock(entry, note.unwrap_or_default());
    let posted = mailspace.pump_post_action(None, need, &request)?;
    Ok(UnlockResult {
        need: need.to_owned(),
        record: posted.record,
        id: posted.id,
        entry: posted.request.word,
    })
}

#[cfg(test)]
#[path = "unlock_test.rs"]
mod tests;
