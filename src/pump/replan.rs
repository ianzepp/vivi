//! Operator replan: `vivi pump replan`.
//!
//! The CLI posts a replan action on the need's open pump record
//! ([`crate::mailspace::PumpActionRequest`]); the flow applies it at its next
//! tick (`pump::flow::operator_actions`), restarting the need's live run at
//! its plan step. A need with no open pump record is refused here.
//!
//! [`replan`] returns what it posted and does not print. [`run`] prints it.

use std::fmt;
use std::path::Path;

use crate::VivariumError;
use crate::cli::PumpReplanCommand;
use crate::mailspace::{Mailspace, PumpActionRequest};
use crate::pump::control::project_home;
use crate::pump::store::{DB_FILENAME, PumpStore};

/// The replan [`replan`] posted. Display is the line the CLI prints.
///
/// `need` is the handle the caller named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplanResult {
    pub need: String,
    /// The open pump record the action was posted on.
    pub record: String,
    /// The action letter's content id.
    pub id: String,
}

impl fmt::Display for ReplanResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "replan {} requested; the pump applies it at its next tick",
            self.need
        )
    }
}

/// `vivi pump replan <NEED>`: post a replan action on the need's open pump
/// record, for the running pump to apply at its next tick. Nothing is printed.
///
/// # Errors
/// Returns an error for a mailspace without `pump.db`, or a need with no open
/// pump record.
pub fn replan(project: &Path, need: &str) -> Result<ReplanResult, VivariumError> {
    let home = project_home(project)?;
    let path = home.join(DB_FILENAME);
    if PumpStore::open_read_only(&path)?.is_none() {
        return Err(VivariumError::Message(format!(
            "no pump.db at {}",
            path.display()
        )));
    }
    let mailspace = Mailspace::discover(Some(project))?;
    if mailspace.pump_open_record(need)?.is_none() {
        return Err(VivariumError::Message(format!(
            "need {need} has no open pump record, so there is no run to replan; open a record with `vivi pump send --need {need}` first"
        )));
    }
    let posted = mailspace.pump_post_action(None, need, &PumpActionRequest::replan())?;
    Ok(ReplanResult {
        need: need.to_owned(),
        record: posted.record,
        id: posted.id,
    })
}

/// `vivi pump replan`: [`replan`], then print the result.
///
/// # Errors
/// Returns whatever [`replan`] returns.
pub fn run(project: &Path, cmd: &PumpReplanCommand) -> Result<(), VivariumError> {
    let posted = replan(project, &cmd.need)?;
    println!("{posted}");
    Ok(())
}
