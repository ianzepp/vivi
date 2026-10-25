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
//! - pool jobs still `pending` past [`STALE_PENDING_SECS`].
//!
//! Steps held for an operator answer have no reader yet and stay zero.

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
    }

    /// A stable one-line summary, used to tell a new alarm from one the mind
    /// already woke for. It carries the newest envelope failure's time, not
    /// the windowed count, so an old failure ageing out does not look new.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        format!(
            "{} verdict={} locked={} held={} red_main={} repair={} envelope_at={} stale_pending={}",
            self.root.display(),
            self.verdict,
            self.locked_needs,
            self.held_steps,
            self.red_main,
            self.repair_alarm,
            self.last_envelope_failure_at
                .map_or_else(|| String::from("none"), |at| at.to_string()),
            self.stale_pending_jobs,
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
    let path = cfg.home_dir(&mailspace.root).join(DB_FILENAME);
    let Some(store) = PumpStore::open_read_only(&path)? else {
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
    fold_jobs(state.jobs.values(), now, signals);
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

/// Live runs waiting at a lock-class node, judged against the shipped flow
/// definition the same way `vivi pump unlock` finds the step to release.
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
