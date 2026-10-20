//! Operator withdraw: `vivi pump withdraw`, both halves.
//!
//! A pump need normally closes through the mail board
//! (`board::reconcile_done_needs`). A need with no mail thread (the synthetic
//! `pumpfix-*` repair needs of Vivi 15 exist only in `pump.db` and plan files)
//! cannot be closed there, so the operator closes it here. The CLI queues a
//! request in `pump.db`; the pump applies it once per tick:
//!
//! - every unfinished unit of the need is withdrawn, stuck ones included;
//! - the need joins `closed_needs`, so nothing re-plans or dispatches it and
//!   the need-audit, need-merge and landing steps skip it;
//! - its need-branch records are dropped. Branch cleanup jobs require a
//!   verified landing, which a withdrawn need never has, so the remote
//!   `vivi/need/<id>` branch is left for manual deletion and the log says so;
//! - an exception of kind `need-withdrawn` records the act.
//!
//! An unknown need id drops the request with a log line.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::VivariumError;
use crate::cli::PumpWithdrawCommand;
use crate::pump::control::project_home;
use crate::pump::host::Ctx;
use crate::pump::state::{State, Status};
use crate::pump::store::{DB_FILENAME, PumpStore, WithdrawRequest};

/// Apply every pending withdraw request. A missing pump database means there
/// is no state to withdraw from; an unreadable one is logged once and skipped.
pub fn apply_pending(ctx: &Ctx, st: &mut State) {
    let path = ctx.home.join(DB_FILENAME);
    let requests = match PumpStore::open_read_only(&path) {
        Ok(Some(store)) => match store.pending_withdraw_requests() {
            Ok(requests) => requests,
            Err(err) => {
                ctx.log(&format!("withdraw: cannot read requests: {err}"));
                return;
            }
        },
        Ok(None) => return,
        Err(err) => {
            ctx.log(&format!("withdraw: cannot read {}: {err}", path.display()));
            return;
        }
    };
    if requests.is_empty() {
        return;
    }
    for request in &requests {
        apply_one(ctx, st, request);
    }
    mark_applied(ctx, &path, &requests);
}

/// Whether the state has anything for `need`: a plan record, a unit or a
/// need-branch record.
fn known(st: &State, need: &str) -> bool {
    st.needs.contains_key(need)
        || st.units.values().any(|unit| unit.need == need)
        || crate::pump::need::all(st).map_or(true, |records| records.iter().any(|r| r.need == need))
}

/// Withdraw one need, or drop the request when the need is unknown.
fn apply_one(ctx: &Ctx, st: &mut State, request: &WithdrawRequest) {
    let need = request.need.as_str();
    if !known(st, need) {
        ctx.log(&format!("withdraw {need} dropped: unknown need"));
        return;
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
    forget_branches(ctx, st, need);
    let text = format!("withdrew need {need}: {count} unit(s) ({})", request.reason);
    ctx.log(&text);
    ctx.log(&format!(
        "withdraw {need}: the mail need is not closed; close it on the board if it has one"
    ));
    ctx.exception("need-withdrawn", need, &text);
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

/// Mark every processed request applied, in one read-write open at the end so
/// a tick that dies mid-way re-applies rather than loses a request.
fn mark_applied(ctx: &Ctx, path: &Path, requests: &[WithdrawRequest]) {
    let mut store = match PumpStore::open(path) {
        Ok(store) => store,
        Err(err) => {
            ctx.log(&format!("withdraw: cannot open {}: {err}", path.display()));
            return;
        }
    };
    let at = ctx.now();
    for request in requests {
        if let Err(err) = store.mark_withdraw_applied(request.request_id, at) {
            ctx.log(&format!("withdraw: cannot mark request applied: {err}"));
        }
    }
}

/// `vivi pump withdraw <NEED>... --reason <TEXT>`: queue one request per need
/// for the running pump to apply at its next tick.
///
/// # Errors
/// Returns [`VivariumError::Message`] for an empty reason or a mailspace
/// without a pump database, and whatever opening or writing the database
/// fails with.
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
    let mut store = PumpStore::open(&path)?;
    for need in &cmd.needs {
        store.add_withdraw_request(need, cmd.reason.trim(), now())?;
        println!("withdraw {need} requested; the pump applies it at its next tick");
    }
    Ok(())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}
