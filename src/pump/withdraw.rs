//! Operator withdraw: `vivi pump withdraw`.
//!
//! The CLI closes each need itself `withdrawn` while its pump record is still
//! open, as the pump identity (a close that travels to every member), then
//! posts a withdraw action on that record
//! ([`crate::mailspace::PumpActionRequest`]). The flow applies the action at
//! its next tick (`pump::flow::operator_actions`): a need with a live run is
//! withdrawn, one with none has its record closed `withdrawn`.

use std::collections::BTreeMap;
use std::path::Path;

use crate::VivariumError;
use crate::cli::PumpWithdrawCommand;
use crate::mailspace::want_state::need_closed;
use crate::mailspace::{Mailspace, PumpActionRequest};
use crate::pump::config::PumpConfig;
use crate::pump::control::project_home;
use crate::pump::store::{DB_FILENAME, PumpStore};

/// `vivi pump withdraw <NEED>... --reason <TEXT>`: post one withdraw action
/// per need on its open pump record, for the running pump to apply at its
/// next tick. Every need is checked first, so a need with no open record
/// refuses the whole command before anything is posted.
///
/// # Errors
/// Returns [`VivariumError::Message`] for an empty reason, a mailspace
/// without a pump database, or a need with no open pump record, and whatever
/// posting an action fails with.
pub fn run(project: &Path, cmd: &PumpWithdrawCommand) -> Result<(), VivariumError> {
    if cmd.reason.trim().is_empty() {
        return Err(VivariumError::Message(String::from(
            "withdraw needs a non-empty --reason",
        )));
    }
    let home = project_home(project)?;
    let path = home.join(DB_FILENAME);
    if PumpStore::open_read_only(&path)?.is_none() {
        return Err(VivariumError::Message(format!(
            "no pump.db at {}",
            path.display()
        )));
    }
    let mailspace = Mailspace::discover(Some(project))?;
    for need in &cmd.needs {
        if mailspace.pump_open_record(need)?.is_none() {
            return Err(VivariumError::Message(format!(
                "need {need} has no open pump record, so there is no dispatch to withdraw; close the need on the board, or open a record with `vivi pump send --need {need}` first"
            )));
        }
    }
    let pump = mailspace.config.pump.as_ref().map_or_else(
        || PumpConfig::default().reply_from,
        |p| p.reply_from.clone(),
    );
    let verdict = BTreeMap::from([(String::from("verdict"), String::from("withdrawn"))]);
    for need in &cmd.needs {
        let closed = close_withdrawn(&mailspace, &pump, need, cmd.reason.trim(), &verdict)?;
        mailspace.pump_post_action(None, need, &PumpActionRequest::withdraw(cmd.reason.trim()))?;
        let state = if closed {
            "need closed withdrawn"
        } else {
            "need already closed"
        };
        println!("withdraw {need} requested; {state}; the pump applies the rest at its next tick");
    }
    Ok(())
}

/// Close `need` `withdrawn` as the pump identity unless it is already closed.
/// Returns whether this call closed it. The pump record is still open, so the
/// close is an event letter that syncs.
fn close_withdrawn(
    mailspace: &Mailspace,
    pump: &str,
    need: &str,
    reason: &str,
    verdict: &BTreeMap<String, String>,
) -> Result<bool, VivariumError> {
    // Closed by the rule a want's state uses: a folder `done` alone can be an
    // assignee's report that nobody accepted, and that is still open.
    let already_closed = {
        let storage = mailspace.storage()?;
        need_closed(&storage, need)?
    };
    if already_closed {
        return Ok(false);
    }
    mailspace.move_item_outcome(pump, need, "done", Some(reason), "need done", Some(verdict))?;
    Ok(true)
}
