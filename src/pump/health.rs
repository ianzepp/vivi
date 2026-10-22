//! `vivi pump health`: a read-only health verdict over one project's pump.
//!
//! Built for monitors that may not run the pump's commands: it reads
//! `pump.db` read-only inside one read transaction, `status.txt` and
//! `monitor-cycles.jsonl` by their metadata and text, and finds the writer
//! from `/proc/locks` without touching the pump lock (see [`writer`]). It
//! never writes, never migrates, and never takes a lock the pump waits on.

mod render;
mod verdict;
mod views;
pub(crate) mod writer;

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::error::VivariumError;
use crate::mailspace::Mailspace;

use super::closeout::{self, CloseoutRow};
use super::cycle::{self, TickRecord};
use super::report::{self, PumpRead, Snapshot};
use super::store::SCHEMA_VERSION;

pub use render::render;
pub use verdict::{Signals, Verdict, decide};
pub use views::{BlockedView, ChangedSince, ExceptionView, MergeView, Merges, UnitChange};

/// The default lookback for exceptions when no `since` is given.
pub const DEFAULT_WINDOW_SECS: u64 = 3600;

/// What a caller asks for.
#[derive(Debug, Clone, Default)]
pub struct HealthOptions {
    /// Epoch seconds: report what changed after this (a previous `as_of`).
    pub since: Option<u64>,
    /// Override the stale threshold, in seconds.
    pub stale_after: Option<u64>,
    /// Epoch seconds to judge against; `None` is the clock.
    pub now: Option<u64>,
}

/// How the writer was found.
#[derive(Debug, Clone, Serialize)]
pub struct WriterView {
    /// `running`, `not_running` or `unknown`.
    pub state: &'static str,
    /// `proc_locks` (exact) or `tick_freshness` (inferred).
    pub source: &'static str,
    pub pid: Option<u32>,
}

/// The newest tick receipt and its age.
#[derive(Debug, Clone, Serialize)]
pub struct TickView {
    pub id: String,
    pub started_at: u64,
    pub ended_at: Option<u64>,
    pub duration_secs: Option<u64>,
    pub outcome: Option<String>,
    pub age_secs: u64,
}

/// The whole answer; the JSON body of the server's pump health route.
#[derive(Debug, Clone, Serialize)]
pub struct PumpHealth {
    pub verdict: Verdict,
    pub reasons: Vec<String>,
    pub as_of: u64,
    pub control: Option<String>,
    pub shutdown_requested: bool,
    pub writer: WriterView,
    pub tick_secs: u64,
    pub stale_after_secs: u64,
    pub last_tick: Option<TickView>,
    pub status_age_secs: Option<u64>,
    /// The pump.db schema this binary reads.
    pub schema: i64,
    pub units: std::collections::BTreeMap<String, usize>,
    pub merges: Merges,
    pub blocked: Vec<BlockedView>,
    pub planner_parked: Vec<String>,
    pub baseline_red: Vec<String>,
    pub complete_open_needs: Vec<CloseoutRow>,
    pub exceptions: Vec<ExceptionView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_since: Option<ChangedSince>,
}

/// The default stale threshold: thirty ticks, at least ten minutes.
#[must_use]
pub fn default_stale_after(tick_secs: u64) -> u64 {
    tick_secs.saturating_mul(30).max(600)
}

/// Judge the pump of the project at `project`.
///
/// # Errors
/// Returns [`VivariumError`] when no mailspace is found at `project`. Every
/// pump-side problem (no `[pump]` table, no database, a schema this binary
/// does not read) is an `unknown` verdict, not an error.
pub fn health(project: &Path, opts: &HealthOptions) -> Result<PumpHealth, VivariumError> {
    let mailspace = Mailspace::discover(Some(project))?;
    let now = opts.now.unwrap_or_else(epoch_now);
    let Some(cfg) = mailspace.config.pump.clone() else {
        let mut out = blank(now, 0, opts);
        out.reasons
            .push(String::from("no [pump] table in .vivi/mailspace.toml"));
        return Ok(out);
    };
    let home = cfg.home_dir(&mailspace.root);
    let mut out = blank(now, cfg.tick, opts);
    out.last_tick = last_tick(&home, now);
    out.status_age_secs = age_of(&home.join("status.txt"), now);
    out.writer = writer_view(&home, out.last_tick.as_ref(), out.stale_after_secs);
    match report::read(&home)? {
        PumpRead::Missing => out
            .reasons
            .push(String::from("no pump.db at the pump home")),
        PumpRead::Unreadable(why) => out.reasons.push(format!("pump.db unreadable: {why}")),
        PumpRead::Ready(snapshot) => fill(&mut out, &home, &mailspace, &snapshot, opts)?,
    }
    Ok(out)
}

/// An `unknown` answer with no reasons yet and no pump state.
fn blank(now: u64, tick_secs: u64, opts: &HealthOptions) -> PumpHealth {
    PumpHealth {
        verdict: Verdict::Unknown,
        reasons: Vec::new(),
        as_of: now,
        control: None,
        shutdown_requested: false,
        writer: WriterView {
            state: "unknown",
            source: "none",
            pid: None,
        },
        tick_secs,
        stale_after_secs: opts
            .stale_after
            .unwrap_or_else(|| default_stale_after(tick_secs)),
        last_tick: None,
        status_age_secs: None,
        schema: SCHEMA_VERSION,
        units: std::collections::BTreeMap::new(),
        merges: Merges::default(),
        blocked: Vec::new(),
        planner_parked: Vec::new(),
        baseline_red: Vec::new(),
        complete_open_needs: Vec::new(),
        exceptions: Vec::new(),
        changed_since: None,
    }
}

/// Everything read from a ready snapshot, then the verdict.
fn fill(
    out: &mut PumpHealth,
    home: &Path,
    mailspace: &Mailspace,
    snapshot: &Snapshot,
    opts: &HealthOptions,
) -> Result<(), VivariumError> {
    let state = snapshot.state();
    let now = out.as_of;
    if let Ok(control) = super::control::read(home) {
        out.control = Some(control.mode.to_string());
        out.shutdown_requested = control.shutdown_requested;
    }
    out.units = state.counts();
    out.merges = views::merges(state);
    out.blocked = views::blocked(state, now);
    out.planner_parked = state.board.planner_failed.clone();
    out.baseline_red = state.baseline.clone().unwrap_or_default();
    out.complete_open_needs = closeout::complete_open(mailspace, snapshot)?;
    let window = opts
        .since
        .unwrap_or_else(|| now.saturating_sub(DEFAULT_WINDOW_SECS));
    out.exceptions = views::exceptions(snapshot.exception_rows(), window);
    out.changed_since = opts
        .since
        .map(|since| views::changed_since(state, snapshot.exception_rows(), since));
    let (verdict, reasons) = decide(&signals(out));
    out.verdict = verdict;
    out.reasons = reasons;
    Ok(())
}

fn signals(out: &PumpHealth) -> Signals {
    let count = |name: &str| out.units.get(name).copied().unwrap_or(0);
    Signals {
        control: out.control.clone(),
        writer_running: match out.writer.state {
            "running" => Some(true),
            "not_running" => Some(false),
            _ => None,
        },
        tick_age: out.last_tick.as_ref().map(|tick| tick.age_secs),
        status_age: out.status_age_secs,
        stale_after: out.stale_after_secs,
        stuck: count("stuck"),
        quarantined: count("quarantined"),
        needs_replan: count("needs-replan"),
        planner_parked: out.planner_parked.len(),
        red: out.baseline_red.len(),
        stall_exceptions: out
            .exceptions
            .iter()
            .filter(|row| row.kind.starts_with("stall-"))
            .count(),
    }
}

/// The exact writer from `/proc/locks`, else an inference from the newest
/// tick: activity within `stale_after` counts as running.
fn writer_view(home: &Path, tick: Option<&TickView>, stale_after: u64) -> WriterView {
    if let Some(holder) = writer::holder(&home.join("lock")) {
        return WriterView {
            state: if holder.is_some() {
                "running"
            } else {
                "not_running"
            },
            source: "proc_locks",
            pid: holder,
        };
    }
    let fresh = tick.is_some_and(|tick| tick.age_secs <= stale_after);
    WriterView {
        state: if fresh { "running" } else { "not_running" },
        source: "tick_freshness",
        pid: None,
    }
}

/// The newest tick receipt; an unreadable receipt file reads as none.
fn last_tick(home: &Path, now: u64) -> Option<TickView> {
    let Ok(mut records) = cycle::read(home) else {
        return None;
    };
    let record: TickRecord = records.pop()?;
    let latest = record.ended_at.unwrap_or(record.started_at);
    Some(TickView {
        age_secs: now.saturating_sub(latest),
        id: record.id,
        started_at: record.started_at,
        ended_at: record.ended_at,
        duration_secs: record.duration_secs,
        outcome: record.outcome,
    })
}

fn age_of(path: &Path, now: u64) -> Option<u64> {
    let Ok(modified) = fs::metadata(path).and_then(|meta| meta.modified()) else {
        return None;
    };
    let at = modified
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    Some(now.saturating_sub(at))
}

fn epoch_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

#[cfg(test)]
#[path = "health_test.rs"]
mod tests;
