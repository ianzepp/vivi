//! The dispatch, wait and reconcile engines behind `vivi pool dispatch | wait |
//! reconcile`. The commands and the live self-test of `vivi pool doctor
//! --test` both call these, so the test exercises the same code a real
//! dispatch uses.
//!
//! The task is the unit: a dispatch and its result are recorded on it, and the
//! task itself is never closed here.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::collect::{self, Missing, Outcome, Pacing, RoundLine};
use super::dispatch::{self, Launch};
use super::pools::PoolSet;
use super::progress::Beat;
use super::provider::PoolProvider;
use super::record::{self, DispatchRecord, OpenDispatch};
use super::run;
use super::slot::{PoolId, SlotStatus};
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

/// The open dispatches that are `set`'s business: those of its pools, and
/// (when no `--pool` was given) those of pools no longer configured.
///
/// # Errors
/// Returns an error if the event log cannot be read.
pub fn scoped_open(space: &Mailspace, set: &PoolSet) -> Result<Vec<OpenDispatch>, VivariumError> {
    Ok(open_dispatches(space)?
        .into_iter()
        .filter(|d| set.covers(&d.record.pool))
        .collect())
}

/// Claim a free slot of the pool `id`, start `plan` on it, and record the dispatch on its
/// task. When the record cannot be written the slot is freed again.
///
/// # Errors
/// Returns an error if no slot is free, the provider fails, or the record
/// cannot be written.
pub fn submit(
    space: &Mailspace,
    provider: &dyn PoolProvider,
    id: &PoolId,
    launch: &Launch,
    pacing: &Pacing,
) -> Result<DispatchRecord, VivariumError> {
    let plan = launch.plan;
    let made = dispatch::dispatch(provider, id, launch, epoch_secs(), pacing)?;
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
        "{}  {}  {}  {}  {}",
        line.task, line.state, line.pool, line.slot, line.detail
    )
}

/// How `wait` paces and when it gives up.
pub struct WaitOptions<'a> {
    pub limit: Duration,
    /// Pause between rounds.
    pub poll: Duration,
    pub pacing: Pacing,
    /// How often `notice` gets a heartbeat while nothing has settled.
    pub heartbeat: Duration,
    /// True once the caller wants to stop waiting (an interrupt).
    pub cancelled: &'a dyn Fn() -> bool,
}

/// The line that says what a wait is about to do.
fn start_text(open: &[OpenDispatch], options: &WaitOptions) -> String {
    let held: Vec<String> = open
        .iter()
        .map(|d| format!("{} on {}", d.task, d.record.slot))
        .collect();
    format!(
        "waiting for {} dispatch(es): {} (checks every {}s, gives up after {}s)",
        open.len(),
        held.join(", "),
        options.poll.as_secs(),
        options.limit.as_secs()
    )
}

/// The heartbeat: what is being waited on and for how long, from the latest
/// round.
#[must_use]
pub fn beat_text(lines: &[RoundLine]) -> String {
    let parts: Vec<String> = lines
        .iter()
        .map(|l| match l.state {
            "error" => format!("{} on {}  cannot poll: {}", l.task, l.slot, l.detail),
            _ => format!("{} on {}  running {} budget", l.task, l.slot, l.detail),
        })
        .collect();
    format!("waiting  {}", parts.join("; "))
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
/// any open dispatch. `notice` gets the start line, a heartbeat every
/// `options.heartbeat`, and each warning once; `watch` keeps every poll error
/// for the caller, whether or not the wait succeeds.
///
/// # Errors
/// Returns an error if a named task has no open dispatch, nothing is open,
/// nothing settled within the limit, the wait was cancelled, or a record
/// could not be written.
pub fn wait(
    space: &Mailspace,
    set: &PoolSet,
    wanted: &[String],
    options: &WaitOptions,
    watch: &mut collect::ErrorWatch,
    notice: &mut dyn FnMut(&str),
) -> Result<Waited, VivariumError> {
    check_wanted(space, set, wanted)?;
    let started = Instant::now();
    let mut beat = Beat::new(options.heartbeat);
    let mut last_error = None;
    let mut announced = false;
    loop {
        let open = wanted_open(space, set, wanted)?;
        if open.is_empty() {
            return Ok(Waited::NothingOpen);
        }
        if !announced {
            notice(&start_text(&open, options));
            announced = true;
        }
        let mut record = recorder(space);
        let pacing = &options.pacing;
        let missing = Missing::Report;
        let lines = collect::run_round(set, &open, epoch_secs(), pacing, missing, &mut record)?;
        watch
            .observe(&lines, epoch_secs())
            .iter()
            .for_each(|n| notice(n));
        let settled: Vec<RoundLine> = lines.iter().filter(|l| l.settled()).cloned().collect();
        if !settled.is_empty() {
            return Ok(Waited::Settled(settled));
        }
        last_error = lines
            .iter()
            .find(|l| l.state == "error")
            .cloned()
            .or(last_error);
        if (options.cancelled)() {
            return Err(VivariumError::Message("interrupted".into()));
        }
        if started.elapsed() >= options.limit {
            return Err(timeout_error(options.limit.as_secs(), last_error.as_ref()));
        }
        if beat.due(started.elapsed()) {
            notice(&beat_text(&lines));
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

fn wanted_open(
    space: &Mailspace,
    set: &PoolSet,
    wanted: &[String],
) -> Result<Vec<OpenDispatch>, VivariumError> {
    Ok(scoped_open(space, set)?
        .into_iter()
        .filter(|d| wanted.is_empty() || wanted.contains(&d.task))
        .collect())
}

/// Named tasks must have an open dispatch now; with none named, something
/// must be open.
fn check_wanted(space: &Mailspace, set: &PoolSet, wanted: &[String]) -> Result<(), VivariumError> {
    let open = scoped_open(space, set)?;
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
    /// Lines for standard output, in order (`items` as text, then the idle
    /// note).
    pub out: Vec<String>,
    /// What the round did, one entry per task or slot, in order.
    pub items: Vec<RoundLine>,
    /// True when no dispatch was open.
    pub idle: bool,
    /// Lines for standard error.
    pub warnings: Vec<String>,
    /// Dispatches that could not be polled, plus slots that could not be freed.
    pub failures: usize,
    /// The first poll error's text.
    pub first_error: Option<String>,
}

impl ReconcileReport {
    fn add(&mut self, line: RoundLine) {
        self.out.push(line_text(&line));
        self.items.push(line);
    }

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
/// that no task holds, in every pool of `set`. A dispatch whose pool is no
/// longer configured is closed as lost. One pool that cannot be listed is a
/// problem to report; the others are still reconciled.
///
/// # Errors
/// Returns an error if a record cannot be written.
pub fn reconcile(
    space: &Mailspace,
    set: &PoolSet,
    pacing: &Pacing,
) -> Result<ReconcileReport, VivariumError> {
    let open = scoped_open(space, set)?;
    let now = epoch_secs();
    let missing = Missing::Close;
    let lines = collect::run_round(set, &open, now, pacing, missing, &mut recorder(space))?;
    let mut report = ReconcileReport {
        out: lines.iter().map(line_text).collect(),
        items: lines.clone(),
        failures: lines.iter().filter(|l| l.state == "error").count(),
        first_error: lines
            .iter()
            .find(|l| l.state == "error")
            .map(|l| l.detail.clone()),
        ..ReconcileReport::default()
    };
    for member in &set.members {
        let here: Vec<OpenDispatch> = open
            .iter()
            .filter(|d| d.record.pool == member.id.pool)
            .cloned()
            .collect();
        match run::pool_status(member.provider, member.id) {
            Ok(slots) => {
                free_stale_claims(member.provider, &slots, &here, now, pacing, &mut report);
                stop_orphans(member.provider, &slots, &here, pacing, &mut report);
            }
            Err(e) => {
                report.failures += 1;
                report.first_error.get_or_insert_with(|| e.to_string());
                report
                    .warnings
                    .push(format!("warning: cannot list pool {}: {e}", member.id.pool));
            }
        }
    }
    if lines.is_empty() {
        report.out.push("no open dispatches".into());
        report.idle = true;
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
    for slot in collect::stale_claims(slots, open, now, provider.setup_grace_secs()) {
        let task = slot.task.as_deref().unwrap_or_default();
        match collect::release(provider, &slot.id, task, pacing) {
            Ok(()) => report.add(RoundLine {
                task: task.to_string(),
                pool: slot.pool.clone(),
                slot: slot.name.clone(),
                state: "stale-claim",
                detail: "freed (no open dispatch)".to_string(),
            }),
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
            Ok(()) => report.add(RoundLine {
                task: "-".to_string(),
                pool: slot.pool.clone(),
                slot: slot.name.clone(),
                state: "orphan-stopped",
                detail: "stopped (running with no task)".to_string(),
            }),
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
