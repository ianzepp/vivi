//! Operator withdraw: `vivi pump withdraw`, both halves.
//!
//! A pump need normally closes through the mail board
//! (`board::reconcile_done_needs`). The operator closes one here when it
//! should stop. The CLI closes the need itself `withdrawn` while its pump
//! record is still open, as the pump identity (a close that travels to every
//! member), then posts a withdraw action on that record
//! ([`crate::mailspace::PumpActionRequest`]); the pump applies it once
//! ([`crate::pump::action`]):
//!
//! - every unfinished unit of the need is withdrawn, stuck ones included;
//! - the need joins `closed_needs`, so nothing re-plans or dispatches it;
//! - the need record is flagged `withdrawn`, so the amendment scan never
//!   reopens or revises it on a later thread change;
//! - an exception of kind `need-withdrawn` records the act;
//! - afterwards the pump closes the need's pump record `withdrawn`.
//!
//! A need the pump holds no state for (its planner has not run) still has its
//! record closed: there is nothing to withdraw but the dispatch.

use std::collections::BTreeMap;
use std::path::Path;

use crate::VivariumError;
use crate::cli::PumpWithdrawCommand;
use crate::mailspace::want_state::need_closed;
use crate::mailspace::{Mailspace, PumpActionRequest};
use crate::pump::config::PumpConfig;
use crate::pump::control::project_home;
use crate::pump::host::Ctx;
use crate::pump::state::{State, Status};
use crate::pump::store::{DB_FILENAME, PumpStore};

/// Whether the state has anything for `need`: a plan record or a unit.
fn known(st: &State, need: &str) -> bool {
    st.needs.contains_key(need) || st.units.values().any(|unit| unit.need == need)
}

/// Withdraw one need in `st` and return the text the pump replies on its
/// record when it closes it `withdrawn`. A need the state holds nothing for
/// is only logged: its dispatch still ends.
pub fn apply_one(ctx: &Ctx, st: &mut State, need: &str, reason: &str) -> String {
    if !known(st, need) {
        let text = format!("withdrew need {need}: the pump held no plan for it ({reason})");
        ctx.log(&format!(
            "withdraw {need}: unknown need; closing its record"
        ));
        return text;
    }
    let now = ctx.now();
    let mut count = 0usize;
    for unit in st.units.values_mut().filter(|unit| unit.need == need) {
        if crate::pump::board::unfinished_closed_need_unit(unit) || unit.status == Status::Stuck {
            unit.status = Status::Withdrawn;
            unit.since = now;
            unit.ready_at = None;
            count += 1;
        }
    }
    if !st.closed_needs.iter().any(|closed| closed == need) {
        st.closed_needs.push(need.to_owned());
    }
    if let Some(record) = st.needs.get_mut(need) {
        record.withdrawn = true;
    }
    let text = format!("withdrew need {need}: {count} unit(s) ({reason})");
    ctx.log(&text);
    ctx.exception("need-withdrawn", need, &text);
    text
}

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
