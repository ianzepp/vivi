//! Intake: start a run for each need with an open pump record.
//!
//! Each tick the phase resumes runs left `starting`, then ranks the needs that
//! have an open pump record, no run in flight and every build prerequisite
//! landed, and starts as many as the `[pump.flow] max_live_runs` cap allows,
//! at most one repair-kind run per target. Repair-kind runs neither count
//! toward the cap nor wait for it. The addressee of the need plays no part.
//!
//! A build whose prerequisites have not landed is assessed every tick, even
//! with the cap full, so its wait projection stays current: it enters no
//! ranking and consumes no slot, and a lower-ranked eligible need fills the
//! slot meanwhile.
//!
//! A build parked `restarting` with a saved entry and no successor is a queued
//! admission candidate of its own: it ranks with the fresh needs by the same
//! key, its parent takes no cap slot, and the successor starts here at the
//! saved entry after a fresh assessment, never in the restart phase.
//!
//! Assumption: the target of a repair-kind need is read from the `target` of
//! the open pump record's front matter, which is written together with
//! `kind = "repair"`.

use std::collections::BTreeMap;

use chrono::DateTime;

use crate::pump::flow::advance::restart::{RestartResult, finish_deferred};
use crate::pump::flow::advance::start::{resume, start};
use crate::pump::flow::build_prereq::assess;
use crate::pump::flow::def::FlowKind;
use crate::pump::flow::handlers::{FlowMail, Registry, TickPhase};
use crate::pump::flow::index::{RunRow, run_state};
use crate::pump::flow::kind;
use crate::pump::flow::load::load;
use crate::pump::flow::mail::{MailReader, NeedStatus};
use crate::pump::flow::main_rung::record_target;
use crate::pump::flow::rank::need_key;
use crate::pump::flow::tick::{FLOW_INVALID, raise_once};
use crate::pump::host::Ctx;
use crate::pump::jobs::JobStore;
use crate::pump::store::PumpStore;
use crate::pump::store::build_waits::BuildWait;
use vivi_error::VivariumError;

/// Exception kind: a run stayed `starting` too long.
pub const START_STUCK: &str = "flow-run-start-stuck";

/// Seconds a run may stay `starting` before the alarm.
const START_STUCK_SECS: u64 = 600;

/// Exception kind: a need has an open pump record the flow cannot act on.
pub const RECORD_UNUSABLE: &str = "flow-record-unusable";

/// Exception kind: a pump record, a need root letter or an operator action
/// letter is a legacy or invalid letter. The pump starts no run on it.
pub const LETTER_INVALID: &str = "flow-letter-invalid";

/// Registry hook: the intake phase.
pub(crate) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.phase(Box::new(Intake));
}

/// An open-record need that may get a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub need: String,
    pub repair: bool,
    /// The repair target ("" when none or not a repair need).
    pub target: String,
    pub priority: i64,
    /// Record age key: the record's epoch seconds (older sorts first), or
    /// `u64::MAX` when the date does not parse.
    pub age: u64,
    /// The saved restart this candidate finishes instead of starting run one.
    pub restart: Option<Parked>,
}

/// A build parked `restarting`: the run its successor replaces and the entry
/// the successor starts at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Parked {
    pub run: u32,
    pub entry: String,
}

/// The needs to start, in rank order.
///
/// `active` is the number of non-repair runs already starting, live or
/// restarting; `busy_targets` the targets of repair-kind runs already in
/// flight. With a `cap` the selection stops picking ordinary candidates when
/// `active` plus the ordinary picks reach it. A repair-kind candidate is never
/// stopped by the cap and does not count toward it; one whose target is busy
/// (or already picked) is skipped.
#[must_use]
pub(crate) fn select(
    mut candidates: Vec<Candidate>,
    active: usize,
    cap: Option<usize>,
    busy_targets: &[String],
) -> Vec<String> {
    candidates.sort_by_key(|c| need_key(c.repair, c.priority, c.age, &c.need));
    let mut busy: Vec<String> = busy_targets.to_vec();
    let mut picked = Vec::new();
    let mut ordinary = 0;
    for candidate in candidates {
        if candidate.repair {
            if busy.contains(&candidate.target) {
                continue;
            }
            busy.push(candidate.target.clone());
        } else {
            if cap.is_some_and(|cap| active + ordinary >= cap) {
                break;
            }
            ordinary += 1;
        }
        picked.push(candidate.need);
    }
    picked
}

struct Intake;

impl<S: JobStore, M: FlowMail> TickPhase<S, M> for Intake {
    fn name(&self) -> &'static str {
        "intake"
    }

    fn run(
        &self,
        ctx: &Ctx,
        _st: &mut S,
        store: &PumpStore,
        mail: &mut M,
    ) -> Result<(), VivariumError> {
        resume_starting(ctx, store, mail);
        sweep_waits(ctx, store, mail)?;
        let (active, busy) = in_flight(store, mail)?;
        let cap = ctx
            .cfg
            .flow
            .max_live_runs
            .map(|n| usize::try_from(n).unwrap_or(usize::MAX));
        let found = candidates(ctx, store, mail)?;
        let parked: BTreeMap<String, Parked> = found
            .iter()
            .filter_map(|c| c.restart.clone().map(|p| (c.need.clone(), p)))
            .collect();
        for need in select(found, active, cap, &busy) {
            match parked.get(&need) {
                Some(saved) => start_parked(ctx, store, mail, &need, saved)?,
                None => {
                    start(ctx, store, mail, &need, None)?;
                }
            }
        }
        Ok(())
    }
}

/// Start the successor a parked restart asked for, at its saved entry, after a
/// fresh prerequisite assessment; a restart still blocked stays queued.
fn start_parked<M: FlowMail>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &mut M,
    need: &str,
    saved: &Parked,
) -> Result<(), VivariumError> {
    let Some(old) = store
        .runs_of(need)?
        .into_iter()
        .find(|r| r.run == saved.run)
    else {
        ctx.log(&format!(
            "flow intake: need {need} r{} is gone, so its restart is dropped",
            saved.run
        ));
        return Ok(());
    };
    if finish_deferred(ctx, store, mail, &old, &saved.entry)? == RestartResult::Waiting {
        ctx.log(&format!(
            "flow intake: need {need} r{} still waits before its successor",
            saved.run
        ));
    }
    Ok(())
}

/// Resume every `starting` run; past the stuck limit raise the alarm once.
fn resume_starting<M: FlowMail>(ctx: &Ctx, store: &PumpStore, mail: &mut M) {
    let runs = match store.runs_in_state(run_state::STARTING) {
        Ok(runs) => runs,
        Err(error) => {
            ctx.log(&format!("flow intake: cannot list starting runs: {error}"));
            return;
        }
    };
    for run in runs {
        let Err(error) = resume(ctx, store, mail, &run) else {
            continue;
        };
        let unit = format!("{} r{}", run.need, run.run);
        ctx.log(&format!("flow intake: resume of {unit} failed: {error}"));
        let since = match store.run_state_at(&run.need, run.run) {
            Ok(Some(at)) => u64::try_from(at).unwrap_or(u64::MAX),
            _ => u64::MAX,
        };
        if since != u64::MAX && ctx.now().saturating_sub(since) > START_STUCK_SECS {
            raise_once(
                ctx,
                START_STUCK,
                &unit,
                &format!(
                    "need {} run {} is stuck starting: {error}",
                    run.need, run.run
                ),
            );
        }
    }
}

/// The count of non-repair runs under the cap and the targets of repair-kind
/// runs in flight (repair runs use no cap slot).
///
/// A restarting parent takes no slot of its own: a committed successor is
/// counted as that successor's run, and a parked build is counted only when
/// its queue position is picked.
fn in_flight<M: FlowMail>(
    store: &PumpStore,
    mail: &M,
) -> Result<(usize, Vec<String>), VivariumError> {
    let mut active = 0;
    let mut busy = Vec::new();
    for state in [run_state::STARTING, run_state::LIVE, run_state::RESTARTING] {
        for run in store.runs_in_state(state)? {
            if run.kind == FlowKind::Repair.name() {
                busy.push(target_of(mail, &run.need)?);
            } else if state != run_state::RESTARTING || !parent_takes_no_slot(store, &run)? {
                active += 1;
            }
        }
    }
    Ok((active, busy))
}

/// Whether a restarting run occupies no slot of its own: its successor is
/// already committed, or it is a build parked for intake at a saved entry.
fn parent_takes_no_slot(store: &PumpStore, run: &RunRow) -> Result<bool, VivariumError> {
    if has_successor(store, run)? {
        return Ok(true);
    }
    Ok(run.kind == FlowKind::Build.name() && store.restart_entry(&run.need, run.run)?.is_some())
}

/// Whether a newer starting or live run of the same need exists, so the
/// restart already started its successor.
fn has_successor(store: &PumpStore, run: &RunRow) -> Result<bool, VivariumError> {
    Ok(store.runs_of(&run.need)?.iter().any(|row| {
        row.run > run.run && matches!(row.state.as_str(), run_state::STARTING | run_state::LIVE)
    }))
}

/// Drop every queued wait whose need is done, whose record closed or was
/// replaced, or that is no longer a queued build. A wait whose lifecycle
/// cannot be read is left in place: a failed read never erases an
/// observation.
fn sweep_waits<M: FlowMail>(ctx: &Ctx, store: &PumpStore, mail: &M) -> Result<(), VivariumError> {
    for wait in store.build_waits()? {
        match stale(mail, &wait) {
            Ok(true) => store.remove_build_wait(&wait.need)?,
            Ok(false) => {}
            Err(error) => {
                ctx.log(&format!(
                    "flow intake: cannot read build wait of {}: {error}",
                    wait.need
                ));
            }
        }
    }
    Ok(())
}

/// Whether the queued wait no longer describes a queued build.
fn stale<M: MailReader>(mail: &M, wait: &BuildWait) -> Result<bool, VivariumError> {
    if mail.need_state(&wait.need)? != NeedStatus::Open {
        return Ok(true);
    }
    let Some((handle, body)) = mail.open_record(&wait.need)? else {
        return Ok(true);
    };
    if handle != wait.record {
        return Ok(true);
    }
    let marks = mail.need_marks(&wait.need)?;
    Ok(kind::of(Some(&body), &marks) != FlowKind::Build)
}

/// The needs with an open record, an open need state and a slot they may
/// fill, with their rank inputs.
fn candidates<M: FlowMail>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &M,
) -> Result<Vec<Candidate>, VivariumError> {
    let priorities: BTreeMap<String, i64> = store.need_priorities()?.into_iter().collect();
    let mut found = Vec::new();
    warn_unreadable(ctx, mail)?;
    for (need, date) in mail.open_record_needs()? {
        if let Some(candidate) = candidate(ctx, store, mail, &need, &date, &priorities)? {
            found.push(candidate);
        }
    }
    Ok(found)
}

/// One open-record need as a candidate, or `None` when the flow cannot act on
/// it this tick: a done need, an invalid letter, an unusable record, a run in
/// flight with no parked restart, or a blocked build.
fn candidate<M: FlowMail>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &M,
    need: &str,
    date: &str,
    priorities: &BTreeMap<String, i64>,
) -> Result<Option<Candidate>, VivariumError> {
    let runs = store.runs_of(need)?;
    let record = MailReader::open_record(mail, need)?;
    let handle = record.as_ref().map_or("", |(handle, _)| handle.as_str());
    if mail.need_state(need)? != NeedStatus::Open {
        raise_once(ctx, RECORD_UNUSABLE, need, &done_text(need, handle));
        return Ok(None);
    }
    if let Some(text) = mail.need_letter_error(need)? {
        raise_once(ctx, LETTER_INVALID, need, &text);
        return Ok(None);
    }
    let marks = mail.need_marks(need)?;
    let body = record.map(|(_, body)| body);
    let flow_kind = kind::of(body.as_deref(), &marks);
    if let Err(error) = load(flow_kind) {
        raise_once(ctx, FLOW_INVALID, need, &error.to_string());
        return Ok(None);
    }
    let repair = flow_kind == FlowKind::Repair;
    let target = match body.as_deref() {
        Some(text) if repair => target_line(text),
        _ => String::new(),
    };
    if repair && !known_target(ctx, need, &target) {
        return Ok(None);
    }
    let restart = parked_restart(store, &runs, flow_kind)?;
    if restart.is_none() && has_run_in_flight(&runs) {
        return Ok(None);
    }
    if !admitted(ctx, store, mail, need) {
        return Ok(None);
    }
    Ok(Some(Candidate {
        need: need.to_string(),
        repair,
        target,
        priority: priorities.get(need).copied().unwrap_or(0),
        age: age_of(date),
        restart,
    }))
}

/// The saved restart a build is parked on: a `restarting` run with no
/// committed successor and a recorded entry. A missing entry is not parked, so
/// the restart phase keeps raising that corruption.
fn parked_restart(
    store: &PumpStore,
    runs: &[RunRow],
    flow_kind: FlowKind,
) -> Result<Option<Parked>, VivariumError> {
    if flow_kind != FlowKind::Build {
        return Ok(None);
    }
    let Some(old) = runs.iter().find(|row| row.state == run_state::RESTARTING) else {
        return Ok(None);
    };
    if has_successor(store, old)? {
        return Ok(None);
    }
    Ok(store
        .restart_entry(&old.need, old.run)?
        .map(|entry| Parked {
            run: old.run,
            entry,
        }))
}

/// The diagnostic of a done need that still holds an open pump record.
fn done_text(need: &str, handle: &str) -> String {
    format!(
        "need {need} has open pump record {handle} but the need is done: expected an open need; \
         close the record with `vivi pump done {handle}`"
    )
}

/// Whether the shared build assessment admits `need` this tick. A blocked
/// build stays queued with its wait projection; a read error is raised for
/// this need alone and unrelated needs are still admitted.
fn admitted<M: FlowMail>(ctx: &Ctx, store: &PumpStore, mail: &M, need: &str) -> bool {
    match assess(ctx, store, mail, need) {
        Ok(gate) => gate.eligible,
        Err(error) => {
            ctx.log(&format!("flow intake: cannot assess build {need}: {error}"));
            false
        }
    }
}

/// Raise once for every pump record or operator action letter that does not
/// parse; each names no need and is skipped.
fn warn_unreadable<M: FlowMail>(ctx: &Ctx, mail: &M) -> Result<(), VivariumError> {
    for (handle, text) in mail.unreadable_records()? {
        raise_once(ctx, LETTER_INVALID, &handle, &text);
    }
    for (handle, text) in mail.unreadable_actions()? {
        raise_once(ctx, LETTER_INVALID, &handle, &text);
    }
    Ok(())
}

/// Whether a repair record names a configured target; raise once when not.
fn known_target(ctx: &Ctx, need: &str, target: &str) -> bool {
    let targets: Vec<String> = ctx.cfg.target_configs().into_keys().collect();
    if targets.iter().any(|id| id == target) {
        return true;
    }
    let actual = if target.is_empty() { "none" } else { target };
    raise_once(
        ctx,
        RECORD_UNUSABLE,
        need,
        &format!(
            "repair need {need}: expected a `target` in the record front matter naming one of {}; actual: {actual}",
            targets.join(", ")
        ),
    );
    false
}

fn has_run_in_flight(runs: &[RunRow]) -> bool {
    runs.iter().any(|run| {
        matches!(
            run.state.as_str(),
            run_state::STARTING | run_state::LIVE | run_state::FINISHING | run_state::RESTARTING
        )
    })
}

fn target_of<M: FlowMail>(mail: &M, need: &str) -> Result<String, VivariumError> {
    Ok(MailReader::open_record(mail, need)?
        .map(|(_, body)| target_line(&body))
        .unwrap_or_default())
}

/// The target a record body names; empty when absent.
fn target_line(body: &str) -> String {
    record_target(body).unwrap_or_default()
}

/// Epoch seconds of an RFC 3339 or RFC 2822 date; `u64::MAX` when neither parses.
fn age_of(date: &str) -> u64 {
    match DateTime::parse_from_rfc3339(date).or_else(|_| DateTime::parse_from_rfc2822(date)) {
        Ok(at) => u64::try_from(at.timestamp()).unwrap_or(u64::MAX),
        Err(_) => u64::MAX,
    }
}

#[cfg(test)]
#[path = "intake_test.rs"]
mod tests;

#[cfg(test)]
#[path = "intake_prereq_test.rs"]
mod prereq_tests;

#[cfg(test)]
#[path = "intake_mail_test.rs"]
mod mail_tests;

#[cfg(test)]
#[path = "intake_restart_test.rs"]
mod restart_tests;
