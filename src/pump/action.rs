//! The pump's side of operator actions on pump records.
//!
//! `vivi pump requeue`, `withdraw`, `replan` and `recover` post an action
//! letter on the need's open pump record
//! ([`crate::mailspace::PumpActionRequest`]). Each tick the pump reads the
//! actions posted on open records that it has not applied yet and applies
//! them in this order: requeues, then recovers, then withdraws, then replans.
//!
//! # Exactly once
//!
//! An applied action's letter id is saved in `st.board.actions`, which
//! `PumpStore::save` writes with the rest of the state in one transaction.
//! The state change and the "applied" mark are therefore durable together or
//! not at all: a tick that dies before saving replays the action against the
//! unchanged state, and a tick that saved never sees the action again. The
//! pump saves right after applying a batch and before it touches a record.
//!
//! A withdraw or replan also owes its record a close (`withdrawn`, or
//! `superseded` plus a fresh record). The debt is part of the saved mark
//! (`owes`), settled after the save, and cleared once paid. The settling
//! steps each check before they act ([`crate::pump::record::settle_action`]),
//! so a tick that dies between the save and the close resumes the debt on the
//! next tick instead of losing it or paying it twice.
//!
//! # A new dispatch
//!
//! A pump record the pump has not run the need under before is a new
//! dispatch. Opening one is the retry of a need the pump gave up on: the
//! planner state that parked the need is cleared and its stuck units are
//! requeued with a fresh attempt budget, so the new record is planned or
//! resumed instead of closing `gave-up` again at once. The need audit's own
//! hold is not cleared: it waits for an operator ruling.

use std::collections::BTreeSet;

use crate::mailspace::{Mailspace, PumpAction, PumpActionKind, PumpRecord, PumpState};
use crate::pump::flow::operator_actions::has_active_run;
use crate::pump::host::Ctx;
use crate::pump::record::{self, SUPERSEDED, WITHDRAWN};
use crate::pump::replan::Outcome;
use crate::pump::requeue::RequeueRequest;
use crate::pump::state::{ActionMark, State, Status};
use crate::pump::store::{DB_FILENAME, PumpStore};

/// Take the new pump records, apply the unapplied actions posted on open
/// records, save, and settle the record closes the actions owe. Nothing here
/// fails the tick: a read that cannot run is logged and the next tick tries
/// again.
pub fn apply_pending(ctx: &Ctx, st: &mut State) {
    let Some(mailspace) = record::open_mailspace(ctx) else {
        return;
    };
    let open = match mailspace.pump_list(None, Some(PumpState::Open)) {
        Ok(open) => open,
        Err(error) => {
            ctx.log(&format!("pump record list failed: {error}"));
            return;
        }
    };
    revive_new_records(ctx, st, &open);
    let applied: BTreeSet<String> = st.board.actions.keys().cloned().collect();
    let actions = match mailspace.pump_pending_actions(&applied) {
        Ok(actions) => without_flow_runs(ctx, without_answers(actions)),
        Err(error) => {
            ctx.log(&format!("pump actions unreadable: {error}"));
            return;
        }
    };
    if !actions.is_empty() {
        apply_all(ctx, st, &actions);
        if !persist(ctx, st) {
            return;
        }
    }
    settle_owed(ctx, st, &mailspace);
}

/// Answers belong to the flow engine, which keeps its own applied set.
fn without_answers(actions: Vec<PumpAction>) -> Vec<PumpAction> {
    actions
        .into_iter()
        .filter(|action| action.request.kind != PumpActionKind::Answer)
        .collect()
}

/// Actions on a need with an active flow run belong to the flow's
/// operator-actions phase: leave them unmarked here. A store that cannot be
/// read leaves every action for the next tick.
fn without_flow_runs(ctx: &Ctx, actions: Vec<PumpAction>) -> Vec<PumpAction> {
    if actions.is_empty() {
        return actions;
    }
    let store = match PumpStore::open_read_only(&ctx.home.join(DB_FILENAME)) {
        Ok(Some(store)) => store,
        Ok(None) => return actions,
        Err(error) => {
            ctx.log(&format!("actions wait: the run index cannot open: {error}"));
            return Vec::new();
        }
    };
    let mut kept = Vec::new();
    for action in actions {
        match has_active_run(&store, &action.need) {
            Ok(false) => kept.push(action),
            Ok(true) => {}
            Err(error) => ctx.log(&format!(
                "action {} waits: runs of {} unreadable: {error}",
                action.id, action.need
            )),
        }
    }
    kept
}

/// Apply `actions` (oldest first) to `st` and mark each applied. An action
/// that cannot be applied yet (a replan whose old plan file will not move)
/// is left unmarked, so the next tick tries it again.
fn apply_all(ctx: &Ctx, st: &mut State, actions: &[PumpAction]) {
    let requeues: Vec<RequeueRequest> = actions
        .iter()
        .filter(|action| action.request.kind == PumpActionKind::Requeue)
        .filter_map(|action| requeue_request(ctx, st, action))
        .collect();
    crate::pump::requeue::apply_batch(ctx, st, &requeues);
    for kind in [
        PumpActionKind::Requeue,
        PumpActionKind::Recover,
        PumpActionKind::Withdraw,
        PumpActionKind::Replan,
    ] {
        for action in actions.iter().filter(|action| action.request.kind == kind) {
            if let Some(mark) = apply_one(ctx, st, action) {
                st.board.actions.insert(action.id.clone(), mark);
            }
        }
    }
}

/// Apply one non-requeue action, returning the mark to save, or `None` when
/// it must be tried again. Requeues were applied as a batch: their mark owes
/// nothing.
fn apply_one(ctx: &Ctx, st: &mut State, action: &PumpAction) -> Option<ActionMark> {
    let need = action.need.as_str();
    let (owes, detail) = match action.request.kind {
        PumpActionKind::Requeue => (String::new(), String::new()),
        PumpActionKind::Answer => return None,
        PumpActionKind::Recover => {
            let (target, reason) = (action.request.target.as_deref(), &action.request.reason);
            crate::pump::recover::apply_one(ctx, st, need, target, reason);
            (String::new(), String::new())
        }
        PumpActionKind::Withdraw => {
            let reason = action.request.reason.as_str();
            let text = crate::pump::withdraw::apply_one(ctx, st, need, reason);
            (WITHDRAWN.to_owned(), text)
        }
        PumpActionKind::Replan => {
            match crate::pump::replan::apply_one(ctx, st, need, action.request.force) {
                Ok(Outcome::Superseded) => (
                    SUPERSEDED.to_owned(),
                    String::from(
                        "operator replan: this plan was superseded; a fresh record carries the new plan",
                    ),
                ),
                Ok(Outcome::Refused | Outcome::Rearmed) => (String::new(), String::new()),
                Err(error) => {
                    ctx.log(&format!(
                        "replan: {need} not applied; the action stays pending: {error}"
                    ));
                    return None;
                }
            }
        }
    };
    Some(ActionMark {
        record: action.record.clone(),
        need: action.need.clone(),
        kind: action.request.kind.as_str().to_owned(),
        owes,
        detail,
    })
}

/// The requeue one action asks for, or `None` (logged) when the unit belongs
/// to another need than the record the action was posted on.
fn requeue_request(ctx: &Ctx, st: &State, action: &PumpAction) -> Option<RequeueRequest> {
    let unit = action.request.unit.clone().unwrap_or_default();
    if let Some(owner) = st.units.get(&unit).map(|found| found.need.as_str())
        && owner != action.need
    {
        ctx.log(&format!(
            "requeue {unit} refused: it belongs to need {owner}, not {}",
            action.need
        ));
        return None;
    }
    Some(RequeueRequest {
        unit,
        attempts: action.request.attempts,
        fresh: action.request.fresh,
        refund: action.request.refund,
        reason: action.request.reason.clone(),
    })
}

/// Save the state, with the applied marks, before any record is touched.
/// False when it cannot be saved: the record effects wait, so a crash can
/// only ever replay an action whole.
fn persist(ctx: &Ctx, st: &State) -> bool {
    let path = ctx.home.join(DB_FILENAME);
    match PumpStore::open(&path).and_then(|mut store| store.save(st)) {
        Ok(()) => true,
        Err(error) => {
            ctx.log(&format!(
                "actions: cannot save state at {}; record changes wait: {error}",
                path.display()
            ));
            false
        }
    }
}

/// Close the records that applied actions still owe a close.
fn settle_owed(ctx: &Ctx, st: &mut State, mailspace: &Mailspace) {
    for mark in st
        .board
        .actions
        .values_mut()
        .filter(|mark| !mark.owes.is_empty())
    {
        if record::settle_action(ctx, mailspace, mark) {
            mark.owes.clear();
            mark.detail.clear();
        }
    }
}

/// Note the pump record each open need runs under. A record that differs from
/// the one the pump last ran the need under is a new dispatch: clear the
/// planner state that parked the need and requeue its stuck units fresh. A
/// need the pump sees for the first time is only noted, so the records
/// `vivi pump adopt` opens for in-flight work do not disturb it.
fn revive_new_records(ctx: &Ctx, st: &mut State, open: &[PumpRecord]) {
    let mut requeues = Vec::new();
    for record in open {
        let previous = st
            .board
            .records
            .insert(record.need.clone(), record.handle.clone());
        if previous.is_none_or(|seen| seen == record.handle) {
            continue;
        }
        ctx.log(&format!(
            "pump record {} is a new dispatch of need {}; clearing what parked it",
            record.handle, record.need
        ));
        clear_parked(st, &record.need);
        requeues.extend(stuck_units(st, &record.need, &record.handle));
    }
    crate::pump::requeue::apply_batch(ctx, st, &requeues);
}

/// Forget that the planner gave up on `need`.
fn clear_parked(st: &mut State, need: &str) {
    st.board.planner_failed.retain(|failed| failed != need);
    st.board.planner_fails.remove(need);
    st.board.planner_retry_at.remove(need);
    st.board
        .planner_fails
        .remove(&crate::pump::plan::invalid_key(need));
    st.board.plan_findings.remove(need);
}

/// A fresh-attempt requeue for each of `need`'s stuck units.
fn stuck_units(st: &State, need: &str, record: &str) -> Vec<RequeueRequest> {
    st.units
        .iter()
        .filter(|(_, unit)| unit.need == need && unit.status == Status::Stuck)
        .map(|(uid, _)| RequeueRequest {
            unit: uid.clone(),
            attempts: None,
            fresh: true,
            refund: false,
            reason: format!("new pump record {record}"),
        })
        .collect()
}

#[cfg(test)]
#[path = "action_test.rs"]
mod tests;

#[cfg(test)]
#[path = "action_flow_test.rs"]
mod flow_tests;
