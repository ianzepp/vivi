//! Operator replan: `vivi pump replan`, both halves.
//!
//! The CLI posts a replan action on the need's open pump record
//! ([`crate::mailspace::PumpActionRequest`]); the pump applies it once
//! ([`crate::pump::action`]). A replan supersedes the current plan, withdraws
//! units that have not started live work, and clears the need's parked
//! planner state, so the board runs a fresh planner over the whole need
//! thread. Merged units remain merged. Afterwards the pump closes the need's
//! open record `superseded` and opens a fresh one, so each record carries
//! exactly one plan.

use std::fs;
use std::path::Path;

use crate::VivariumError;
use crate::cli::PumpReplanCommand;
use crate::mailspace::{Mailspace, PumpActionRequest};
use crate::pump::control::project_home;
use crate::pump::host::Ctx;
use crate::pump::state::{State, Status};
use crate::pump::store::{DB_FILENAME, PumpStore};

/// What applying one replan did to the state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Live work, or a running planner, refused it without `--force`.
    Refused,
    /// An exhausted audit was rearmed; the plan and the record stand.
    Rearmed,
    /// The plan is thrown away; the record is owed a `superseded` close.
    Superseded,
}

/// Apply one replan of `need` to `st`, unless live work requires an explicit
/// force.
///
/// A need the state holds no plan for (its planner failed, or has not run
/// yet) has nothing to throw away, but its parked planner state is still
/// cleared, so replanning is also how an operator revives it. Replaying this
/// after the plan is already gone is harmless: every step is idempotent.
///
/// # Errors
/// Returns an error when the old plan file cannot be moved aside; the state
/// is untouched then, and the action stays unapplied for the next tick.
pub fn apply_one(
    ctx: &Ctx,
    st: &mut State,
    need: &str,
    force: bool,
) -> Result<Outcome, VivariumError> {
    if crate::pump::audit::rearm_operator_replan(ctx, st, need) {
        return Ok(Outcome::Rearmed);
    }
    let units = st
        .needs
        .get(need)
        .map(|record| record.units.clone())
        .unwrap_or_default();
    if !force && let Some(text) = refusal(st, need, &units) {
        ctx.exception("replan-refused", need, &text);
        ctx.log(&text);
        return Ok(Outcome::Refused);
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
    ctx.log(&format!("replan: {need} superseded by operator request"));
    Ok(Outcome::Superseded)
}

/// Why an ordinary (unforced) replan of `need` is refused, if it is.
fn refusal(st: &State, need: &str, units: &[String]) -> Option<String> {
    if let Some((uid, status)) = live_unit(st, units) {
        return Some(format!(
            "unit {uid} is {}; replan refused: use vivi pump replan {need} --force",
            status.as_str()
        ));
    }
    st.board.planning.contains_key(need).then(|| {
        format!(
            "a planner is already running for this need; replan refused: use vivi pump replan {need} --force"
        )
    })
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
    st.board.planner_fails.remove(&format!("{need}.revision"));
    st.board
        .planner_retry_at
        .remove(&format!("{need}.revision"));
    st.board
        .planner_fails
        .remove(&crate::pump::plan::invalid_key(need));
    st.board.plan_findings.remove(need);
}

/// `vivi pump replan <NEED> [--force]`: post a replan action on the need's
/// open pump record, for the running pump to apply at its next tick. An
/// exhausted audit may be retried even when the need has no board
/// `NeedRecord`.
///
/// # Errors
/// Returns an error for an unknown need, live work without `--force`, a
/// running planner without `--force`, a mailspace without `pump.db`, or a
/// need with no open pump record.
pub fn run(project: &Path, cmd: &PumpReplanCommand) -> Result<(), VivariumError> {
    let home = project_home(project)?;
    let path = home.join(DB_FILENAME);
    let st = load_state(&path)?;
    check_command(&st, cmd)?;
    let mailspace = Mailspace::discover(Some(project))?;
    mailspace.pump_post_action(None, &cmd.need, &PumpActionRequest::replan(cmd.force))?;
    println!(
        "replan {} requested; the pump applies it at its next tick",
        cmd.need
    );
    Ok(())
}

/// The refusals the CLI can make from the pump's saved state. A need the
/// pump has no plan for is known when its planner is running or parked.
fn check_command(st: &State, cmd: &PumpReplanCommand) -> Result<(), VivariumError> {
    let need = cmd.need.as_str();
    if crate::pump::audit::operator_replan_allowed(st, need) {
        return Ok(());
    }
    let planned = st.needs.get(need);
    let parked = st.board.planner_failed.iter().any(|failed| failed == need);
    if planned.is_none() && !parked && !st.board.planning.contains_key(need) {
        return Err(VivariumError::Message(format!("unknown need: {need}")));
    }
    if cmd.force {
        return Ok(());
    }
    if let Some((uid, status)) = planned.and_then(|record| live_unit(st, &record.units)) {
        return Err(VivariumError::Message(format!(
            "refused: unit {uid} is {}; use vivi pump replan {need} --force to replan anyway",
            status.as_str()
        )));
    }
    if st.board.planning.contains_key(need) {
        return Err(VivariumError::Message(String::from(
            "refused: a planner is already running for this need; use --force to replan anyway",
        )));
    }
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
