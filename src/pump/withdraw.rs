//! Operator withdraw: `vivi pump withdraw`.
//!
//! The CLI closes each need itself `withdrawn` while its pump record is still
//! open, as the pump identity (a close that travels to every member), then
//! posts a withdraw action on that record
//! ([`crate::mailspace::PumpActionRequest`]). The flow applies the action at
//! its next tick (`pump::flow::operator_actions`): a need with a live run is
//! withdrawn, one with none has its record closed `withdrawn`.
//!
//! [`withdraw`] returns what it posted and does not print. [`run`] prints each
//! result.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use crate::VivariumError;
use crate::cli::PumpWithdrawCommand;
use crate::mailspace::want_state::need_closed;
use crate::mailspace::{Mailspace, PumpActionRequest};
use crate::pump::config::PumpConfig;
use crate::pump::control::project_home;
use crate::pump::store::{DB_FILENAME, PumpStore};

/// Whether this call closed the need.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WithdrawClose {
    /// This call closed the need with verdict `withdrawn`.
    Closed,
    /// The need was already closed. The action was still posted.
    AlreadyClosed,
}

/// One need [`withdraw`] closed (or found already closed) and posted on.
///
/// Display is the line the CLI prints. `need` is the handle the caller named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WithdrawResult {
    pub need: String,
    pub close: WithdrawClose,
    /// The open pump record the action was posted on.
    pub record: String,
    /// The action letter's content id.
    pub id: String,
}

impl fmt::Display for WithdrawResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = match self.close {
            WithdrawClose::Closed => "need closed withdrawn",
            WithdrawClose::AlreadyClosed => "need already closed",
        };
        write!(
            f,
            "withdraw {} requested; {state}; the pump applies the rest at its next tick",
            self.need
        )
    }
}

/// `vivi pump withdraw <NEED>...`: close each need `withdrawn` and post one
/// withdraw action on its open pump record. Every need is checked first, so a
/// need with no open record refuses the whole call before anything is posted.
/// `reason` is trimmed; a blank reason is refused. Nothing is printed.
///
/// # Errors
/// Returns [`VivariumError::Message`] for an empty reason, a mailspace
/// without a pump database, or a need with no open pump record, and whatever
/// posting an action fails with.
pub fn withdraw(
    project: &Path,
    needs: &[String],
    reason: &str,
) -> Result<Vec<WithdrawResult>, VivariumError> {
    let reason = reason.trim();
    if reason.is_empty() {
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
    refuse_closed_records(&mailspace, needs)?;
    post_all(&mailspace, needs, reason)
}

/// `vivi pump withdraw`: [`withdraw`], then print each result.
///
/// # Errors
/// Returns whatever [`withdraw`] returns.
pub fn run(project: &Path, cmd: &PumpWithdrawCommand) -> Result<(), VivariumError> {
    for result in withdraw(project, &cmd.needs, &cmd.reason)? {
        println!("{result}");
    }
    Ok(())
}

fn refuse_closed_records(mailspace: &Mailspace, needs: &[String]) -> Result<(), VivariumError> {
    for need in needs {
        if mailspace.pump_open_record(need)?.is_none() {
            return Err(VivariumError::Message(format!(
                "need {need} has no open pump record, so there is no dispatch to withdraw; close the need on the board, or open a record with `vivi pump send --need {need}` first"
            )));
        }
    }
    Ok(())
}

fn post_all(
    mailspace: &Mailspace,
    needs: &[String],
    reason: &str,
) -> Result<Vec<WithdrawResult>, VivariumError> {
    let pump = mailspace.config.pump.as_ref().map_or_else(
        || PumpConfig::default().reply_from,
        |config| config.reply_from.clone(),
    );
    let verdict = BTreeMap::from([(String::from("verdict"), String::from("withdrawn"))]);
    let mut posted = Vec::with_capacity(needs.len());
    for need in needs {
        posted.push(post_one(mailspace, &pump, need, reason, &verdict)?);
    }
    Ok(posted)
}

fn post_one(
    mailspace: &Mailspace,
    pump: &str,
    need: &str,
    reason: &str,
    verdict: &BTreeMap<String, String>,
) -> Result<WithdrawResult, VivariumError> {
    let closed = close_withdrawn(mailspace, pump, need, reason, verdict)?;
    let action = mailspace.pump_post_action(None, need, &PumpActionRequest::withdraw(reason))?;
    let close = if closed {
        WithdrawClose::Closed
    } else {
        WithdrawClose::AlreadyClosed
    };
    Ok(WithdrawResult {
        need: need.to_owned(),
        close,
        record: action.record,
        id: action.id,
    })
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
