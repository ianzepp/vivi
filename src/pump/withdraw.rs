//! Operator withdraw: `vivi pump withdraw`, both halves.
//!
//! A pump need normally closes through the mail board
//! (`board::reconcile_done_needs`). The operator closes one here when it
//! should stop without closing the need on the board. The CLI posts a
//! withdraw action on the need's open pump record
//! ([`crate::mailspace::PumpActionRequest`]); the pump applies it once
//! ([`crate::pump::action`]):
//!
//! - every unfinished unit of the need is withdrawn, stuck ones included;
//! - the need joins `closed_needs`, so nothing re-plans or dispatches it and
//!   the need-audit, need-merge and landing steps skip it;
//! - the need record is flagged `withdrawn`, so the amendment scan never
//!   reopens or revises it on a later thread change;
//! - its need-branch records are dropped. Branch cleanup jobs require a
//!   verified landing, which a withdrawn need never has, so the remote
//!   `vivi/need/<id>` branch is left for manual deletion and the log says so;
//! - an exception of kind `need-withdrawn` records the act;
//! - afterwards the pump closes the need's pump record `withdrawn`.
//!
//! A need the pump holds no state for (its planner has not run) still has its
//! record closed: there is nothing to withdraw but the dispatch.

use std::path::Path;

use crate::VivariumError;
use crate::cli::PumpWithdrawCommand;
use crate::mailspace::{Mailspace, PumpActionRequest};
use crate::pump::control::project_home;
use crate::pump::host::Ctx;
use crate::pump::state::{State, Status};
use crate::pump::store::{DB_FILENAME, PumpStore};

/// Whether the state has anything for `need`: a plan record, a unit or a
/// need-branch record.
fn known(st: &State, need: &str) -> bool {
    st.needs.contains_key(need)
        || st.units.values().any(|unit| unit.need == need)
        || crate::pump::need::all(st).map_or(true, |records| records.iter().any(|r| r.need == need))
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
    forget_branches(ctx, st, need);
    let text = format!("withdrew need {need}: {count} unit(s) ({reason})");
    ctx.log(&text);
    ctx.log(&format!(
        "withdraw {need}: the mail need is not closed; close it on the board if it has one"
    ));
    ctx.exception("need-withdrawn", need, &text);
    text
}

/// Drop the need's branch records and say which remote branches stay behind.
fn forget_branches(ctx: &Ctx, st: &mut State, need: &str) {
    match crate::pump::need::forget(st, need) {
        Ok(records) => {
            for record in records {
                ctx.log(&format!(
                    "withdraw {need}: dropped branch record {}/{}; remote branch `{}` is left for manual deletion",
                    record.target, record.need, record.branch
                ));
            }
        }
        Err(err) => ctx.log(&format!(
            "withdraw {need}: cannot drop branch records: {err}"
        )),
    }
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
    for need in &cmd.needs {
        mailspace.pump_post_action(None, need, &PumpActionRequest::withdraw(cmd.reason.trim()))?;
        println!("withdraw {need} requested; the pump applies it at its next tick");
    }
    Ok(())
}
