//! Operator replan: `vivi pump replan`.
//!
//! The CLI posts a replan action on the need's open pump record
//! ([`crate::mailspace::PumpActionRequest`]); the flow applies it at its next
//! tick (`pump::flow::operator_actions`), restarting the need's live run at
//! its plan step. A need with no open pump record is refused here.

use std::path::Path;

use crate::VivariumError;
use crate::cli::PumpReplanCommand;
use crate::mailspace::{Mailspace, PumpActionRequest};
use crate::pump::control::project_home;
use crate::pump::store::{DB_FILENAME, PumpStore};

/// `vivi pump replan <NEED>`: post a replan action on the need's open pump
/// record, for the running pump to apply at its next tick.
///
/// # Errors
/// Returns an error for a mailspace without `pump.db`, or a need with no open
/// pump record.
pub fn run(project: &Path, cmd: &PumpReplanCommand) -> Result<(), VivariumError> {
    let home = project_home(project)?;
    let path = home.join(DB_FILENAME);
    if PumpStore::open_read_only(&path)?.is_none() {
        return Err(VivariumError::Message(format!(
            "no pump.db at {}",
            path.display()
        )));
    }
    let mailspace = Mailspace::discover(Some(project))?;
    if mailspace.pump_open_record(&cmd.need)?.is_none() {
        return Err(VivariumError::Message(format!(
            "need {} has no open pump record, so there is no run to replan; open a record with `vivi pump send --need {}` first",
            cmd.need, cmd.need
        )));
    }
    mailspace.pump_post_action(None, &cmd.need, &PumpActionRequest::replan())?;
    println!(
        "replan {} requested; the pump applies it at its next tick",
        cmd.need
    );
    Ok(())
}
