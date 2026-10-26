//! Read-only pump signals for one registered project.
//!
//! `vivi pump health` alone is not enough: it judges the writer and the
//! tick, and says `ok` for a pump whose need is locked at an operator gate
//! or whose plan job has sat pending for hours on zero free slots (seen on
//! the e2e sandbox). So the real probe adds three reads of `pump.db`, all
//! read-only and never under a lock the pump waits on:
//!
//! - live flow runs waiting at a lock-class node (`locked` in the build
//!   flow, `unrepaired` in the repair flow, which also means main is red);
//! - recent `flow-envelope-failed` exceptions;
//! - pool jobs still `pending` past [`STALE_PENDING_SECS`];
//! - steps `running` with no dispatched job while their job has waited past
//!   [`IDLE_STEP_SECS`] (seen live: every dispatch refused by a pool config
//!   error, health still `ok`);
//! - jobs whose dispatch `<home>/log.txt` shows deferred at least
//!   [`DEFERRAL_REPEAT`] times in its tail, while the job is still pending,
//!   and tick steps (such as `dispatch-jobs`) failing that often, recently.
//!
//! Steps held for an operator answer have no reader yet and stay zero.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::VivariumError;
use crate::mailspace::Mailspace;
use crate::pump::flow::collect::ENVELOPE_FAILED;
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::index::StepState;
use crate::pump::health::{self, HealthOptions, Verdict};
use crate::pump::jobs::{JobStatus, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore, SCHEMA_VERSION};

/// A pending pool job older than this is a stall, whatever health says.
pub const STALE_PENDING_SECS: u64 = 30 * 60;
/// A running step whose job has waited this long undispatched is idle.
pub const IDLE_STEP_SECS: u64 = 3 * 60;
/// Deferred dispatches of one pending job, in the log tail, that make a
/// signal. The dispatcher retries every few seconds, so this is quick.
pub const DEFERRAL_REPEAT: usize = 3;
/// How much of the end of `log.txt` the probe reads.
const LOG_TAIL_BYTES: u64 = 64 * 1024;
/// Longest dispatch error kept in a reason.
const DEFERRAL_TEXT: usize = 160;
/// The dispatcher's log marker for a deferred dispatch.
const DEFERRED: &str = "job dispatch deferred ";
/// The tick's log marker for a failed soft step (`dispatch-jobs` and others).
const TICK_FAILED: &str = "tick step ";
/// A failing tick step counts only while one of its failures is among this
/// many last lines of the log, so a fixed problem stops alarming.
const RECENT_LINES: usize = 50;
/// Envelope failures count for this long after they are raised.
pub const ENVELOPE_WINDOW_SECS: u64 = 3600;

/// What the mind knows about one project's pump at one instant.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSignals {
    pub root: PathBuf,
    /// `ok`, `degraded`, `stalled`, or `unknown`, from `pump health`.
    pub verdict: String,
    /// Health reasons and store findings, verbatim. Untrusted text.
    pub reasons: Vec<String>,
    /// Needs whose live run waits at a lock node.
    pub locked_needs: usize,
    /// Flow steps held awaiting an answer (no reader yet: always zero).
    pub held_steps: usize,
    /// Main is red: a repair run waits at `unrepaired`.
    pub red_main: bool,
    /// A repair flow raised its alarm (the same `unrepaired` wait).
    pub repair_alarm: bool,
    /// `flow-envelope-failed` exceptions inside [`ENVELOPE_WINDOW_SECS`].
    pub envelope_failures: usize,
    /// When the newest of those was raised.
    pub last_envelope_failure_at: Option<u64>,
    /// Pool jobs pending for at least [`STALE_PENDING_SECS`].
    pub stale_pending_jobs: usize,
    /// Running steps with no dispatched job, idle past [`IDLE_STEP_SECS`].
    #[serde(default)]
    pub idle_steps: usize,
    /// Pending jobs with repeated deferred dispatches in the log tail, plus
    /// tick steps failing repeatedly and recently.
    #[serde(default)]
    pub deferring_jobs: usize,
}

impl ProjectSignals {
    /// Whether anything here should wake the mind outside its heartbeat.
    #[must_use]
    pub fn alarming(&self) -> bool {
        matches!(self.verdict.as_str(), "degraded" | "stalled")
            || self.locked_needs > 0
            || self.held_steps > 0
            || self.red_main
            || self.repair_alarm
            || self.envelope_failures > 0
            || self.stale_pending_jobs > 0
            || self.idle_steps > 0
            || self.deferring_jobs > 0
    }

    /// A stable one-line summary, used to tell a new alarm from one the mind
    /// already woke for. It carries the newest envelope failure's time, not
    /// the windowed count, so an old failure ageing out does not look new.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        format!(
            "{} verdict={} locked={} held={} red_main={} repair={} envelope_at={} stale_pending={} idle={} deferring={}",
            self.root.display(),
            self.verdict,
            self.locked_needs,
            self.held_steps,
            self.red_main,
            self.repair_alarm,
            self.last_envelope_failure_at
                .map_or_else(|| String::from("none"), |at| at.to_string()),
            self.stale_pending_jobs,
            self.idle_steps,
            self.deferring_jobs,
        )
    }
}

/// A source of project signals.
pub trait Probe {
    /// Probe one project. A probe never fails: a project that cannot be
    /// read is an `unknown` verdict with the error as its reason.
    fn probe(&self, root: &Path) -> ProjectSignals;
}

/// The real probe: `pump health` plus the store reads above.
#[derive(Debug, Clone, Copy, Default)]
pub struct HealthProbe;

impl Probe for HealthProbe {
    fn probe(&self, root: &Path) -> ProjectSignals {
        let now = epoch_now();
        let options = HealthOptions {
            now: Some(now),
            ..HealthOptions::default()
        };
        let mut signals = match health::health(root, &options) {
            Ok(report) => ProjectSignals {
                root: root.to_path_buf(),
                verdict: report.verdict.as_str().to_string(),
                reasons: report.reasons,
                ..ProjectSignals::default()
            },
            Err(error) => {
                return ProjectSignals {
                    root: root.to_path_buf(),
                    verdict: Verdict::Unknown.as_str().to_string(),
                    reasons: vec![error.to_string()],
                    ..ProjectSignals::default()
                };
            }
        };
        if let Err(error) = read_store(root, now, &mut signals) {
            signals
                .reasons
                .push(format!("pump store unreadable: {error}"));
        }
        signals
    }
}

/// Fold the `pump.db` reads into `signals`. A project without a pump or a
/// database adds nothing.
///
/// # Errors
/// Fails when the database exists but cannot be read, or has a schema this
/// binary does not read.
pub fn read_store(
    root: &Path,
    now: u64,
    signals: &mut ProjectSignals,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(Some(root))?;
    let Some(cfg) = mailspace.config.pump.as_ref() else {
        return Ok(());
    };
    let home = cfg.home_dir(&mailspace.root);
    let Some(store) = PumpStore::open_read_only(&home.join(DB_FILENAME))? else {
        return Ok(());
    };
    let version = store.schema_version()?;
    if version != SCHEMA_VERSION {
        return Err(VivariumError::Other(format!(
            "pump.db schema {version}, expected {SCHEMA_VERSION}"
        )));
    }
    read_lock_waits(&store, signals)?;
    let (state, exceptions) = store.load_snapshot()?;
    fold_exceptions(&exceptions, now, signals);
    let jobs: Vec<&PoolJob> = state.jobs.values().collect();
    fold_jobs(jobs.iter().copied(), now, signals);
    read_idle_steps(&store, &jobs, now, signals)?;
    let pending: BTreeSet<&str> = jobs
        .iter()
        .filter(|job| job.status == JobStatus::Pending)
        .map(|job| job.id.as_str())
        .collect();
    fold_deferrals(&log_tail(&home.join("log.txt"))?, &pending, signals);
    Ok(())
}

/// Count `flow-envelope-failed` rows inside the window and keep the newest.
pub fn fold_exceptions(
    rows: &[(u64, String, String, String)],
    now: u64,
    signals: &mut ProjectSignals,
) {
    let recent: Vec<u64> = rows
        .iter()
        .filter(|(at, kind, ..)| {
            kind == ENVELOPE_FAILED && now.saturating_sub(*at) < ENVELOPE_WINDOW_SECS
        })
        .map(|(at, ..)| *at)
        .collect();
    signals.envelope_failures = recent.len();
    signals.last_envelope_failure_at = recent.iter().max().copied();
}

/// Count pool jobs still pending past [`STALE_PENDING_SECS`].
pub fn fold_jobs<'a>(
    jobs: impl Iterator<Item = &'a PoolJob>,
    now: u64,
    signals: &mut ProjectSignals,
) {
    for job in jobs {
        let age = now.saturating_sub(job.queued_at);
        if job.status == JobStatus::Pending && age >= STALE_PENDING_SECS {
            signals.stale_pending_jobs += 1;
            signals
                .reasons
                .push(format!("job {} pending for {age}s", job.id));
        }
    }
}

/// Running steps of live runs with no dispatched job, whose oldest pending
/// job has waited at least [`IDLE_STEP_SECS`]. A running step with no job
/// at all is not judged: `pump.db` keeps no step timestamps to age it by.
///
/// # Errors
/// Fails when the store's runs, steps or step jobs cannot be read.
pub fn read_idle_steps(
    store: &PumpStore,
    jobs: &[&PoolJob],
    now: u64,
    signals: &mut ProjectSignals,
) -> Result<(), VivariumError> {
    // task -> (a job is dispatched, the oldest pending job's queue time)
    let mut tasks: BTreeMap<String, (bool, Option<u64>)> = BTreeMap::new();
    for job in jobs {
        let Some(row) = store.step_job(&job.id)? else {
            continue;
        };
        let entry = tasks.entry(row.task).or_default();
        match job.status {
            JobStatus::Dispatched => entry.0 = true,
            JobStatus::Pending => {
                entry.1 = Some(entry.1.map_or(job.queued_at, |at| at.min(job.queued_at)));
            }
            _ => {}
        }
    }
    for run in store.live_runs()? {
        for step in store.steps_of(&run.need, run.run)? {
            if step.state != StepState::Running {
                continue;
            }
            let Some((false, Some(queued_at))) = tasks.get(&step.task).copied() else {
                continue;
            };
            let age = now.saturating_sub(queued_at);
            if age >= IDLE_STEP_SECS {
                signals.idle_steps += 1;
                signals.reasons.push(format!(
                    "need {} r{} step `{}` running with no dispatched job for {age}s",
                    run.need, run.run, step.node
                ));
            }
        }
    }
    Ok(())
}

/// Count pending jobs deferred at least [`DEFERRAL_REPEAT`] times in `log`
/// (lines `HH:MM:SS job dispatch deferred <job> on <pool>: <error>`). Only
/// jobs still pending count, so a cleared problem stops alarming.
pub fn fold_deferrals(log: &str, pending: &BTreeSet<&str>, signals: &mut ProjectSignals) {
    let mut seen: BTreeMap<&str, (usize, &str, &str)> = BTreeMap::new();
    for line in log.lines() {
        let Some((_, rest)) = line.split_once(DEFERRED) else {
            continue;
        };
        let Some((job, rest)) = rest.split_once(" on ") else {
            continue;
        };
        if !pending.contains(job) {
            continue;
        }
        let (pool, error) = rest.split_once(": ").unwrap_or((rest, ""));
        let entry = seen.entry(job).or_insert((0, pool, error));
        *entry = (entry.0 + 1, pool, error);
    }
    for (job, (count, pool, error)) in seen {
        if count >= DEFERRAL_REPEAT {
            signals.deferring_jobs += 1;
            let error: String = error.chars().take(DEFERRAL_TEXT).collect();
            signals.reasons.push(format!(
                "job {job} dispatch deferred {count} times on {pool}: {error}"
            ));
        }
    }
    fold_tick_failures(log, signals);
}

/// Tick steps that failed at least [`DEFERRAL_REPEAT`] times in `log`, with
/// one failure among its last [`RECENT_LINES`] lines. Each counts as one
/// deferring signal: a failing `dispatch-jobs` step dispatches nothing.
fn fold_tick_failures(log: &str, signals: &mut ProjectSignals) {
    let lines: Vec<&str> = log.lines().collect();
    let recent_from = lines.len().saturating_sub(RECENT_LINES);
    let mut seen: BTreeMap<&str, (usize, usize, &str)> = BTreeMap::new();
    for (index, line) in lines.iter().enumerate() {
        let Some((_, rest)) = line.split_once(TICK_FAILED) else {
            continue;
        };
        let Some((step, error)) = rest.split_once(" failed: ") else {
            continue;
        };
        let entry = seen.entry(step).or_insert((0, index, error));
        *entry = (entry.0 + 1, index, error);
    }
    for (step, (count, last, error)) in seen {
        if count >= DEFERRAL_REPEAT && last >= recent_from {
            signals.deferring_jobs += 1;
            let error: String = error.chars().take(DEFERRAL_TEXT).collect();
            signals
                .reasons
                .push(format!("tick step {step} failed {count} times: {error}"));
        }
    }
}

/// The last [`LOG_TAIL_BYTES`] of `path`, from the first whole line. A
/// missing log is empty.
///
/// # Errors
/// Fails when the log exists but cannot be read.
pub fn log_tail(path: &Path) -> Result<String, VivariumError> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(error) => return Err(error.into()),
    };
    let start = file.metadata()?.len().saturating_sub(LOG_TAIL_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if start == 0 {
        return Ok(text);
    }
    Ok(text
        .split_once('\n')
        .map_or_else(String::new, |(_, whole)| whole.to_string()))
}

/// Live runs waiting at a lock-class node, judged against the shipped flow
/// definition the same way `vivi pump unlock` finds the step to release.
///
/// # Errors
/// Fails when the store's runs or steps cannot be read.
pub fn read_lock_waits(
    store: &PumpStore,
    signals: &mut ProjectSignals,
) -> Result<(), VivariumError> {
    for run in store.live_runs()? {
        let Some(kind) = FlowKind::parse(&run.kind) else {
            continue;
        };
        let def = crate::pump::flow::load::load(kind)?;
        let waiting = store
            .steps_of(&run.need, run.run)?
            .into_iter()
            .find(|step| {
                step.state == StepState::Waiting
                    && def
                        .nodes
                        .iter()
                        .any(|node| node.id == step.node && node.class == NodeClass::Lock)
            });
        let Some(step) = waiting else {
            continue;
        };
        if kind == FlowKind::Repair {
            signals.red_main = true;
            signals.repair_alarm = true;
        } else {
            signals.locked_needs += 1;
        }
        signals.reasons.push(format!(
            "need {} r{} waits at `{}` ({} flow)",
            run.need,
            run.run,
            step.node,
            kind.name()
        ));
    }
    Ok(())
}

fn epoch_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

#[cfg(test)]
#[path = "probe_test.rs"]
mod tests;
