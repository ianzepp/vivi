//! Operator replan: `vivi pump replan`, both halves.
//!
//! The CLI queues a request in `pump.db`; the pump applies it once per tick.
//! A replan supersedes the current plan, withdraws units that have not started
//! live work, and asks the board to run a fresh planner over the whole need
//! thread. Merged units remain merged.

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::VivariumError;
use crate::cli::PumpReplanCommand;
use crate::pump::control::project_home;
use crate::pump::host::Ctx;
use crate::pump::state::{State, Status};
use crate::pump::store::{DB_FILENAME, PumpStore, ReplanRequest};

/// Apply every pending replan request. A missing pump database means there is
/// no state to replan; read failures are logged once and skipped.
pub fn apply_pending(ctx: &Ctx, st: &mut State) {
    let path = ctx.home.join(DB_FILENAME);
    let requests = match PumpStore::open_read_only(&path) {
        Ok(Some(store)) => match store.pending_replan_requests() {
            Ok(requests) => requests,
            Err(err) => {
                ctx.log(&format!("replan: cannot read requests: {err}"));
                return;
            }
        },
        Ok(None) => return,
        Err(err) => {
            ctx.log(&format!("replan: cannot read {}: {err}", path.display()));
            return;
        }
    };
    if requests.is_empty() {
        return;
    }
    let mut processed = Vec::with_capacity(requests.len());
    let mut changed = false;
    for request in &requests {
        match apply_one(ctx, st, request) {
            Ok(request_changed) => {
                processed.push(request.request_id);
                changed |= request_changed;
            }
            Err(err) => ctx.log(&format!(
                "replan: {} not applied; request remains pending: {err}",
                request.need
            )),
        }
    }
    persist_and_mark(ctx, &path, st, &processed, changed);
}

/// Apply one request unless live work requires an explicit force.
fn apply_one(ctx: &Ctx, st: &mut State, request: &ReplanRequest) -> Result<bool, VivariumError> {
    let need = request.need.as_str();
    if crate::pump::audit::rearm_operator_replan(ctx, st, need) {
        return Ok(true);
    }
    let Some(units) = st.needs.get(need).map(|record| record.units.clone()) else {
        ctx.log(&format!("replan: {need} unknown; nothing to supersede"));
        return Ok(false);
    };
    if !request.force {
        if let Some((uid, status)) = live_unit(st, &units) {
            let text = format!(
                "unit {uid} is {}; replan refused: use vivi pump replan {need} --force",
                status.as_str()
            );
            ctx.exception("replan-refused", need, &text);
            ctx.log(&text);
            return Ok(false);
        }
        if st.board.planning.contains_key(need) {
            let text = format!(
                "a planner is already running for this need; replan refused: use vivi pump replan {need} --force"
            );
            ctx.exception("replan-refused", need, &text);
            ctx.log(&text);
            return Ok(false);
        }
    }
    supersede_plan(ctx, need)?;
    let plan_name = format!("{need}.json");
    st.plans_seen.retain(|seen| seen != &plan_name);
    st.needs.remove(need);
    st.board
        .replied
        .retain(|key| !key.starts_with(&format!("{need}:")));
    withdraw_unstarted(st, &units);
    clear_planner_state(st, need);
    queue_board_replan(st, need);
    ctx.log(&format!("replan: {need} superseded by operator request"));
    Ok(true)
}

/// Find the first referenced unit that still has live or unmerged work.
fn live_unit<'a>(st: &State, units: &'a [String]) -> Option<(&'a str, Status)> {
    units.iter().find_map(|uid| {
        st.units
            .get(uid)
            .filter(|unit| live(unit.status))
            .map(|unit| (uid.as_str(), unit.status))
    })
}

/// Statuses whose work must finish before an ordinary replan can proceed.
fn live(status: Status) -> bool {
    matches!(
        status,
        Status::Inflight
            | Status::Delivered
            | Status::Merging
            | Status::Quarantined
            | Status::Splitting
    )
}

/// Move the old plan aside before changing state; a missing plan is allowed.
fn supersede_plan(ctx: &Ctx, need: &str) -> Result<(), VivariumError> {
    let source = ctx.home.join("plans").join(format!("{need}.json"));
    match fs::metadata(&source) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    let superseded = ctx.home.join("plans/superseded");
    fs::create_dir_all(&superseded)?;
    let destination = superseded.join(format!("{need}.{}.json", ctx.now()));
    match fs::rename(&source, destination) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => match fs::metadata(&source) {
            Err(source_error) if source_error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source_error) => Err(source_error.into()),
            Ok(_) => Err(error.into()),
        },
        Err(error) => Err(error.into()),
    }
}

/// Withdraw work that has not started and must not survive a new plan.
fn withdraw_unstarted(st: &mut State, units: &[String]) {
    for uid in units {
        if let Some(unit) = st.units.get_mut(uid) {
            if matches!(
                unit.status,
                Status::Pending | Status::Held | Status::Stuck | Status::NeedsReplan
            ) {
                unit.status = Status::Withdrawn;
            }
        }
    }
}

/// Clear per-need planner bookkeeping so the board can launch the new plan.
fn clear_planner_state(st: &mut State, need: &str) {
    st.board.planner_failed.retain(|failed| failed != need);
    st.board.planner_fails.remove(need);
    st.board.planner_retry_at.remove(need);
    st.board.planning.remove(need);
    st.board.planner_pids.remove(need);
    st.board.revision_planning.remove(need);
}

/// Add the handle to the board's cutoff-bypass set.
fn queue_board_replan(st: &mut State, need: &str) {
    if !st.board.replan.iter().any(|queued| queued == need) {
        st.board.replan.push(need.to_owned());
        st.board.replan.sort();
    }
}

/// Persist changed state before consuming requests, so a crash can safely
/// replay an unmarked request against the durable state.
fn persist_and_mark(ctx: &Ctx, path: &Path, st: &State, processed: &[i64], changed: bool) {
    if processed.is_empty() {
        return;
    }
    let mut store = match PumpStore::open(path) {
        Ok(store) => store,
        Err(err) => {
            ctx.log(&format!("replan: cannot open {}: {err}", path.display()));
            return;
        }
    };
    if changed && let Err(err) = store.save(st) {
        ctx.log(&format!(
            "replan: cannot persist state before marking requests: {err}"
        ));
        return;
    }
    let at = ctx.now();
    for request_id in processed {
        if let Err(err) = store.mark_replan_applied(*request_id, at) {
            ctx.log(&format!("replan: cannot mark request applied: {err}"));
        }
    }
}

/// Queue a replan for the next pump tick. An exhausted audit may be retried
/// even when it has no board `NeedRecord`.
///
/// # Errors
/// Returns an error for an unknown need, live work without `--force`, a
/// running planner without `--force`, or a mailspace without `pump.db`.
pub fn run(project: &Path, cmd: &PumpReplanCommand) -> Result<(), VivariumError> {
    let home = project_home(project)?;
    let path = home.join(DB_FILENAME);
    let st = load_state(&path)?;
    let audit_retry = crate::pump::audit::operator_replan_allowed(&st, &cmd.need);
    let record = match st.needs.get(&cmd.need) {
        Some(record) => record,
        None if audit_retry => return queue_request(&path, &cmd.need, cmd.force),
        None => {
            return Err(VivariumError::Message(format!(
                "unknown need: {}",
                cmd.need
            )));
        }
    };
    if audit_retry {
        return queue_request(&path, &cmd.need, cmd.force);
    }
    if !cmd.force {
        if let Some((uid, status)) = live_unit(&st, &record.units) {
            return Err(VivariumError::Message(format!(
                "refused: unit {uid} is {}; use vivi pump replan {} --force to replan anyway",
                status.as_str(),
                cmd.need
            )));
        }
        if st.board.planning.contains_key(&cmd.need) {
            return Err(VivariumError::Message(String::from(
                "refused: a planner is already running for this need; use --force to replan anyway",
            )));
        }
    }
    queue_request(&path, &cmd.need, cmd.force)
}

/// Save one operator replan request for the next pump tick.
fn queue_request(path: &Path, need: &str, force: bool) -> Result<(), VivariumError> {
    let mut store = PumpStore::open(path)?;
    store.add_replan_request(need, force, now())?;
    println!("replan {need} requested; the pump applies it at its next tick");
    Ok(())
}

/// Load pump state, refusing a mailspace without a database.
fn load_state(path: &Path) -> Result<State, VivariumError> {
    let Some(store) = PumpStore::open_read_only(path)? else {
        return Err(VivariumError::Message(format!(
            "no pump.db at {}",
            path.display()
        )));
    };
    store.load()
}

/// Current epoch second for the durable request row.
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}
