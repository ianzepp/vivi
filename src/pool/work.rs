//! The dispatch, wait and reconcile engines behind `vivi pool dispatch | wait |
//! reconcile`. The commands and the live self-test of `vivi pool doctor
//! --test` both call these, so the test exercises the same code a real
//! dispatch uses.
//!
//! The task is the unit: a dispatch and its result are recorded on it, and the
//! task itself is never closed here.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::collect::{self, Outcome, Pacing, RoundLine};
use super::dispatch;
use super::github::Secret;
use super::job::JobPlan;
use super::provider::PoolProvider;
use super::record::{self, DispatchRecord, OpenDispatch};
use super::run;
use super::slot::SlotStatus;
use crate::error::VivariumError;
use crate::mailspace::Mailspace;

/// Seconds since the Unix epoch.
#[must_use]
pub fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The mailspace's open dispatches.
///
/// # Errors
/// Returns an error if the event log cannot be read.
pub fn open_dispatches(space: &Mailspace) -> Result<Vec<OpenDispatch>, VivariumError> {
    Ok(record::fold_open(&space.pool_events()?))
}

/// Claim a free slot, start `plan` on it, and record the dispatch on its
/// task. When the record cannot be written the slot is freed again.
///
/// # Errors
/// Returns an error if no slot is free, the provider fails, or the record
/// cannot be written.
pub fn submit(
    space: &Mailspace,
    provider: &dyn PoolProvider,
    pool: &str,
    plan: &JobPlan,
    token: &Secret,
    start: &str,
    pacing: &Pacing,
) -> Result<DispatchRecord, VivariumError> {
    let made = dispatch::dispatch(provider, pool, plan, token, start, epoch_secs(), pacing)?;
    let note = record::dispatch_note(&made);
    let text = record::dispatch_text(&made);
    if let Err(e) = space.record_pool_note(&plan.task, "pool_dispatched", &note, &text) {
        if let Err(undo) = collect::release(provider, &made.machine, &plan.task, pacing) {
            eprintln!(
                "warning: could not stop slot {} after the failed record: {undo}",
                made.slot
            );
        }
        return Err(e);
    }
    Ok(made)
}

/// Record a settled dispatch on its task, unless another process already did.
pub fn recorder(
    space: &Mailspace,
) -> impl FnMut(&OpenDispatch, &Outcome) -> Result<bool, VivariumError> + '_ {
    move |dispatch, outcome| {
        let still_open = open_dispatches(space)?
            .iter()
            .any(|o| o.task == dispatch.task && o.record.job == dispatch.record.job);
        if !still_open {
            return Ok(false);
        }
        space.record_pool_note(
            &dispatch.task,
            "pool_collected",
            &record::collected_note(dispatch, outcome),
            &record::collected_text(dispatch, outcome),
        )?;
        Ok(true)
    }
}

/// One line of a round, as the commands print it.
#[must_use]
pub fn line_text(line: &RoundLine) -> String {
    format!(
        "{}  {}  {}  {}",
        line.task, line.state, line.slot, line.detail
    )
}

/// How `wait` paces and when it gives up.
pub struct WaitOptions<'a> {
    pub limit: Duration,
    /// Pause between rounds.
    pub poll: Duration,
    pub pacing: Pacing,
    /// True once the caller wants to stop waiting (an interrupt).
    pub cancelled: &'a dyn Fn() -> bool,
}

/// What `wait` ended with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Waited {
    /// Every named dispatch was already collected.
    NothingOpen,
    /// These dispatches settled this round.
    Settled(Vec<RoundLine>),
}

/// Block until a wanted dispatch settles, collecting it. `wanted` empty means
/// any open dispatch. `notice` gets each warning once.
///
/// # Errors
/// Returns an error if a named task has no open dispatch, nothing is open,
/// nothing settled within the limit, the wait was cancelled, or a record
/// could not be written.
pub fn wait(
    space: &Mailspace,
    provider: &dyn PoolProvider,
    wanted: &[String],
    options: &WaitOptions,
    notice: &mut dyn FnMut(&str),
) -> Result<Waited, VivariumError> {
    check_wanted(space, wanted)?;
    let started = Instant::now();
    let mut last_error = None;
    let mut watch = collect::ErrorWatch::default();
    loop {
        let open = wanted_open(space, wanted)?;
        if open.is_empty() {
            return Ok(Waited::NothingOpen);
        }
        let mut record = recorder(space);
        let pacing = &options.pacing;
        let lines = collect::run_round(provider, &open, epoch_secs(), pacing, &mut record)?;
        watch.observe(&lines).iter().for_each(|n| notice(n));
        let settled: Vec<RoundLine> = lines.iter().filter(|l| l.settled()).cloned().collect();
        if !settled.is_empty() {
            return Ok(Waited::Settled(settled));
        }
        last_error = lines
            .into_iter()
            .find(|l| l.state == "error")
            .or(last_error);
        if (options.cancelled)() {
            return Err(VivariumError::Message("interrupted".into()));
        }
        if started.elapsed() >= options.limit {
            return Err(timeout_error(options.limit.as_secs(), last_error.as_ref()));
        }
        std::thread::sleep(options.poll);
    }
}

fn timeout_error(seconds: u64, last_error: Option<&RoundLine>) -> VivariumError {
    let tail = last_error.map_or(String::new(), |l| {
        format!(" (last error on {}: {})", l.task, l.detail)
    });
    VivariumError::Message(format!("nothing finished within {seconds}s{tail}"))
}

fn wanted_open(space: &Mailspace, wanted: &[String]) -> Result<Vec<OpenDispatch>, VivariumError> {
    Ok(open_dispatches(space)?
        .into_iter()
        .filter(|d| wanted.is_empty() || wanted.contains(&d.task))
        .collect())
}

/// Named tasks must have an open dispatch now; with none named, something
/// must be open.
fn check_wanted(space: &Mailspace, wanted: &[String]) -> Result<(), VivariumError> {
    let open = open_dispatches(space)?;
    if wanted.is_empty() && open.is_empty() {
        return Err(VivariumError::Message(
            "no open dispatches to wait for".into(),
        ));
    }
    let missing: Vec<&str> = wanted
        .iter()
        .filter(|w| !open.iter().any(|d| &d.task == *w))
        .map(String::as_str)
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    Err(VivariumError::Message(format!(
        "no open dispatch for: {}",
        missing.join(", ")
    )))
}

/// What one `reconcile` round did.
#[derive(Debug, Clone, Default)]
pub struct ReconcileReport {
    /// Lines for standard output, in order.
    pub out: Vec<String>,
    /// Lines for standard error.
    pub warnings: Vec<String>,
    /// Dispatches that could not be polled, plus slots that could not be freed.
    pub failures: usize,
    /// The first poll error's text.
    pub first_error: Option<String>,
}

impl ReconcileReport {
    /// The error to exit with, when anything went wrong.
    #[must_use]
    pub fn error(&self) -> Option<VivariumError> {
        if self.failures == 0 {
            return None;
        }
        let tail = self
            .first_error
            .as_ref()
            .map_or(String::new(), |e| format!(" (first: {e})"));
        Some(VivariumError::Message(format!(
            "{} problem(s) while reconciling{tail}",
            self.failures
        )))
    }
}

/// Collect finished dispatches, free stale claims, and stop running slots
/// that no task holds.
///
/// # Errors
/// Returns an error if the provider cannot list the pool or a record cannot
/// be written.
pub fn reconcile(
    space: &Mailspace,
    provider: &dyn PoolProvider,
    pool: &str,
    pacing: &Pacing,
) -> Result<ReconcileReport, VivariumError> {
    let open = open_dispatches(space)?;
    let now = epoch_secs();
    let lines = collect::run_round(provider, &open, now, pacing, &mut recorder(space))?;
    let slots = run::pool_status(provider, pool)?;
    let mut report = ReconcileReport {
        out: lines.iter().map(line_text).collect(),
        failures: lines.iter().filter(|l| l.state == "error").count(),
        first_error: lines
            .iter()
            .find(|l| l.state == "error")
            .map(|l| l.detail.clone()),
        ..ReconcileReport::default()
    };
    free_stale_claims(provider, &slots, &open, now, pacing, &mut report);
    stop_orphans(provider, &slots, &open, pacing, &mut report);
    if lines.is_empty() {
        report.out.push("no open dispatches".into());
    }
    Ok(report)
}

fn free_stale_claims(
    provider: &dyn PoolProvider,
    slots: &[SlotStatus],
    open: &[OpenDispatch],
    now: u64,
    pacing: &Pacing,
    report: &mut ReconcileReport,
) {
    for slot in collect::stale_claims(slots, open, now) {
        let task = slot.task.as_deref().unwrap_or_default();
        match collect::release(provider, &slot.id, task, pacing) {
            Ok(()) => report.out.push(format!(
                "{task}  stale-claim  {}  freed (no open dispatch)",
                slot.name
            )),
            Err(e) => {
                report
                    .warnings
                    .push(format!("warning: could not free {}: {e}", slot.name));
                report.failures += 1;
            }
        }
    }
}

/// Stop running slots that no task holds.
fn stop_orphans(
    provider: &dyn PoolProvider,
    slots: &[SlotStatus],
    open: &[OpenDispatch],
    pacing: &Pacing,
    report: &mut ReconcileReport,
) {
    for slot in collect::orphan_runners(slots, open) {
        match provider
            .machine(&slot.id)
            .and_then(|m| collect::stop_unclaimed(provider, &m, pacing))
        {
            Ok(()) => report.out.push(format!(
                "-  orphan-stopped  {}  stopped (running with no task)",
                slot.name
            )),
            Err(e) => {
                report
                    .warnings
                    .push(format!("warning: could not stop {}: {e}", slot.name));
                report.failures += 1;
            }
        }
    }
}

#[cfg(test)]
#[path = "work_test.rs"]
mod tests;
