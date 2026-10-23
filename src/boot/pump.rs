//! Role-gated pump and pool snapshot for `vivi boot`.
//!
//! Built only from local, cheap sources: a read-only `pump.db`, `status.txt`,
//! mailspace routing, and local-provider slot marks on disk. Remote providers
//! are never contacted.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::pump_slots::{self, LocalSlotMark};
use super::truncate::{
    self, CAP_PUMP_EXCEPTIONS, CAP_PUMP_HELD, CAP_PUMP_INFLIGHT, CAP_PUMP_POOLS, CAP_PUMP_QUEUE,
    CAP_PUMP_STUCK, Truncation,
};
use crate::mailspace::Mailspace;
use crate::pool::config::PoolConfig;
use crate::pump::report::{self, PumpRead};
use crate::pump::state::State;
use crate::routing::{RoleRouting, RoutedRole};
use crate::storage::Storage;

mod build;
mod gate;
pub(super) mod text;

pub use gate::PumpView;

/// Flags that change which pump rows boot includes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BootOptions {
    /// `--role` override; otherwise inferred from a local slot or left unset.
    pub role: Option<String>,
    /// `--pump full|own|none`; unset means the role's default.
    pub pump: Option<PumpView>,
}

/// Local pump state, when this host has a `pump.db` or configured pools.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PumpBoot {
    /// Unit counts by status.
    pub units: BTreeMap<String, usize>,
    /// Needs whose every unit is satisfied.
    pub complete: usize,
    /// Complete needs still open in this mailspace.
    pub complete_but_open: usize,
    /// Units parked for a person.
    pub stuck: usize,
    /// Needs with a post-plan amendment waiting.
    pub amendments: usize,
    /// Exception rows in the pump database.
    pub exceptions: usize,
    /// Planners the board currently has in flight.
    pub planning: usize,
    /// Handles parked after exhausting planner retries.
    pub planner_parked: usize,
    /// Durable operator control mode for this pump home.
    pub control: String,
    /// Whether a writer process currently holds the pump lock.
    pub writer: String,
    /// Whole seconds since `status.txt` was last written, when it exists.
    pub status_age: Option<u64>,
    /// Depth of the snapshot that follows.
    pub view: PumpView,
    /// Role the snapshot was gated for, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Role-gated detail; absent for `--pump none`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<PumpSnapshot>,
}

/// The pump/pool snapshot a fresh session reads from boot.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct PumpSnapshot {
    pub inflight: Vec<PumpUnitRow>,
    pub merge_queue: Vec<PumpUnitRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_wait: Option<String>,
    pub held: Vec<PumpUnitRow>,
    pub needs_ready: Vec<String>,
    pub stuck: Vec<PumpUnitRow>,
    pub exceptions: Vec<PumpExceptionRow>,
    pub lanes: Vec<String>,
    pub pools: Vec<PumpPoolRow>,
}

/// One unit in the snapshot.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PumpUnitRow {
    pub unit: String,
    pub need: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub holder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub held_on: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub after: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_sha: Option<String>,
}

/// One pump exception.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PumpExceptionRow {
    pub kind: String,
    pub unit: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age: Option<String>,
}

/// One configured pool as boot can see it locally.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PumpPoolRow {
    pub name: String,
    pub provider: String,
    pub used: usize,
    pub total: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_of: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_dir: Option<String>,
    pub roles: Vec<String>,
    pub remote: bool,
}

/// Inputs the snapshot builder uses. Kept public so tests can feed a fixture
/// without opening a mailspace.
#[derive(Debug, Clone)]
pub struct PumpSources<'a> {
    pub now: u64,
    pub budget_secs: u64,
    pub routing: &'a RoleRouting,
    pub pools: &'a BTreeMap<String, PoolConfig>,
    pub state: Option<&'a State>,
    pub exception_rows: &'a [(u64, String, String, String)],
    pub status_txt: Option<&'a str>,
    pub slots: &'a [LocalSlotMark],
    pub holders: &'a BTreeMap<String, String>,
    pub open_needs: &'a BTreeSet<String>,
    pub complete_needs: BTreeSet<String>,
    pub pump_to: &'a str,
}

/// Collect the boot pump section. Missing state is absent, never fatal.
///
/// # Errors
/// Returns an error only when `--role` names an unknown identity.
pub fn collect(
    mailspace: &Mailspace,
    open_needs: &BTreeSet<String>,
    opts: &BootOptions,
    now: DateTime<Utc>,
    truncations: &mut Vec<Truncation>,
) -> Result<Option<PumpBoot>, crate::error::VivariumError> {
    let home = pump_home(mailspace);
    let loaded = load_snapshot(&home);
    let slots = pump_slots::scan(&mailspace.config, &mailspace.root);
    let cwd = current_dir();
    let role = resolve_role(mailspace, opts.role.as_deref(), &slots, cwd.as_deref())?;
    if loaded.is_none() && mailspace.config.pools.is_empty() {
        return Ok(None);
    }
    let here = pump_slots::detect_here(&slots, cwd.as_deref());
    let view = gate::choose_view(
        opts.pump,
        role.as_deref(),
        &mailspace.config,
        here.is_some(),
    );
    if view == PumpView::None && loaded.is_none() {
        return Ok(None);
    }
    let boot = assemble(Assemble {
        mailspace,
        open_needs,
        now_epoch: u64::try_from(now.timestamp()).unwrap_or(0),
        loaded: loaded.as_ref(),
        slots: &slots,
        role: role.as_deref(),
        view,
        here,
        truncations,
    });
    Ok(Some(boot))
}

struct Assemble<'a> {
    mailspace: &'a Mailspace,
    open_needs: &'a BTreeSet<String>,
    now_epoch: u64,
    loaded: Option<&'a report::Snapshot>,
    slots: &'a [LocalSlotMark],
    role: Option<&'a str>,
    view: PumpView,
    here: Option<&'a LocalSlotMark>,
    truncations: &'a mut Vec<Truncation>,
}

fn assemble(mut args: Assemble<'_>) -> PumpBoot {
    let counts = args.loaded.map(report::Snapshot::summary);
    let complete_needs = args
        .loaded
        .map(report::Snapshot::complete_needs)
        .unwrap_or_default();
    let complete_but_open = complete_needs.intersection(args.open_needs).count();
    let snapshot = (args.view != PumpView::None).then(|| snapshot_for(&mut args, complete_needs));
    let mut boot = from_counts(
        counts.as_ref(),
        complete_but_open,
        args.view,
        args.role,
        snapshot,
    );
    let home = pump_home(args.mailspace);
    boot.control = crate::pump::control::inspect(&home).map_or_else(
        |_| String::from("unknown"),
        |control| control.mode.to_string(),
    );
    boot.writer = match crate::pump::run::lock_is_held(&home) {
        Ok(true) => String::from("running"),
        Ok(false) => String::from("not running"),
        Err(_) => String::from("unknown"),
    };
    boot.status_age = status_age_seconds(&home);
    boot
}

/// Whole seconds since `<home>/status.txt` was last written, when the file
/// exists and its modification time can be read.
fn status_age_seconds(home: &std::path::Path) -> Option<u64> {
    let modified = match std::fs::metadata(home.join("status.txt")).and_then(|meta| meta.modified())
    {
        Ok(modified) => modified,
        Err(_) => return None,
    };
    Some(
        std::time::SystemTime::now()
            .duration_since(modified)
            .map_or(0, |elapsed| elapsed.as_secs()),
    )
}

fn snapshot_for(args: &mut Assemble<'_>, complete_needs: BTreeSet<String>) -> PumpSnapshot {
    let holders = holders_for(args.mailspace, args.loaded);
    let routing = crate::routing::from_mailspace(&args.mailspace.config);
    let status = read_status_txt(&pump_home(args.mailspace));
    let sources = PumpSources {
        now: args.now_epoch,
        budget_secs: budget_secs(args.mailspace),
        routing: &routing,
        pools: &args.mailspace.config.pools,
        state: args.loaded.map(report::Snapshot::state),
        exception_rows: args.loaded.map_or(&[], report::Snapshot::exception_rows),
        status_txt: status.as_deref(),
        slots: args.slots,
        holders: &holders,
        open_needs: args.open_needs,
        complete_needs,
        pump_to: pump_to(args.mailspace),
    };
    cap_snapshot(
        build::snapshot(&sources, args.view, args.role, args.here),
        args.truncations,
    )
}

fn from_counts(
    counts: Option<&report::StateSummary>,
    complete_but_open: usize,
    view: PumpView,
    role: Option<&str>,
    snapshot: Option<PumpSnapshot>,
) -> PumpBoot {
    PumpBoot {
        units: counts.map(|s| s.units.clone()).unwrap_or_default(),
        complete: counts.map_or(0, |s| s.complete),
        complete_but_open,
        stuck: counts.map_or(0, |s| s.stuck),
        amendments: counts.map_or(0, |s| s.amendments),
        exceptions: counts.map_or(0, |s| s.exceptions),
        planning: counts.map_or(0, |s| s.planning),
        planner_parked: counts.map_or(0, |s| s.planner_parked),
        control: String::from("unknown"),
        writer: String::from("unknown"),
        status_age: None,
        view,
        role: role.map(str::to_string),
        snapshot,
    }
}

fn budget_secs(mailspace: &Mailspace) -> u64 {
    mailspace
        .config
        .pump
        .as_ref()
        .map_or(600, |cfg| cfg.dispatch.budget)
}

fn pump_to(mailspace: &Mailspace) -> &str {
    mailspace
        .config
        .pump
        .as_ref()
        .map_or("hand", |cfg| cfg.to.as_str())
}

fn pump_home(mailspace: &Mailspace) -> PathBuf {
    mailspace.config.pump.as_ref().map_or_else(
        || mailspace.root.join(".vivi").join("pump"),
        |cfg| cfg.home_dir(&mailspace.root),
    )
}

fn load_snapshot(home: &std::path::Path) -> Option<report::Snapshot> {
    match report::read(home) {
        Ok(PumpRead::Ready(snapshot)) => Some(snapshot),
        _ => None,
    }
}

fn read_status_txt(home: &std::path::Path) -> Option<String> {
    Result::ok(std::fs::read_to_string(home.join("status.txt")))
}

fn current_dir() -> Option<PathBuf> {
    Result::ok(std::env::current_dir())
}

fn resolve_role(
    mailspace: &Mailspace,
    flag: Option<&str>,
    slots: &[LocalSlotMark],
    cwd: Option<&std::path::Path>,
) -> Result<Option<String>, crate::error::VivariumError> {
    if let Some(name) = flag {
        return mailspace.resolve_identity(name).map(Some);
    }
    let here = pump_slots::detect_here(slots, cwd);
    Ok(here.and_then(|slot| slot_role(mailspace, slot)))
}

fn slot_role(mailspace: &Mailspace, slot: &LocalSlotMark) -> Option<String> {
    let task = slot.task.as_deref()?;
    holder_of(mailspace, task)
}

fn holders_for(
    mailspace: &Mailspace,
    loaded: Option<&report::Snapshot>,
) -> BTreeMap<String, String> {
    let Some(snapshot) = loaded else {
        return BTreeMap::new();
    };
    let Ok(storage) = mailspace.storage() else {
        return BTreeMap::new();
    };
    let mut holders = BTreeMap::new();
    for unit in snapshot.state().units.values() {
        let Some(task) = unit.task.as_deref() else {
            continue;
        };
        if let Some(role) = lookup_holder(&storage, mailspace, task) {
            holders.insert(task.to_string(), role);
        }
    }
    holders
}

fn holder_of(mailspace: &Mailspace, task: &str) -> Option<String> {
    let Ok(storage) = mailspace.storage() else {
        return None;
    };
    lookup_holder(&storage, mailspace, task)
}

fn lookup_holder(storage: &Storage, mailspace: &Mailspace, task: &str) -> Option<String> {
    let Ok(id) = storage.resolve_message_token(task) else {
        return None;
    };
    let Ok(Some(view)) = storage.message_by_id(&id) else {
        return None;
    };
    if let Ok(name) = mailspace.resolve_identity(&view.account) {
        return Some(name);
    }
    Result::ok(mailspace.resolve_identity(&view.to_addr))
}

fn cap_snapshot(mut snap: PumpSnapshot, truncations: &mut Vec<Truncation>) -> PumpSnapshot {
    snap.inflight = truncate::capped(
        snap.inflight,
        CAP_PUMP_INFLIGHT,
        "pump inflight",
        truncations,
    );
    snap.merge_queue =
        truncate::capped(snap.merge_queue, CAP_PUMP_QUEUE, "pump queue", truncations);
    snap.held = truncate::capped(snap.held, CAP_PUMP_HELD, "pump held", truncations);
    snap.stuck = truncate::capped(snap.stuck, CAP_PUMP_STUCK, "pump stuck", truncations);
    snap.exceptions = truncate::capped(
        snap.exceptions,
        CAP_PUMP_EXCEPTIONS,
        "pump exceptions",
        truncations,
    );
    snap.pools = truncate::capped(snap.pools, CAP_PUMP_POOLS, "pump pools", truncations);
    snap
}

/// Build a snapshot from already-loaded sources (tests).
#[must_use]
pub fn build_snapshot(
    sources: &PumpSources<'_>,
    view: PumpView,
    role: Option<&str>,
    here: Option<&LocalSlotMark>,
) -> PumpSnapshot {
    build::snapshot(sources, view, role, here)
}

/// Default view for a role, used by tests and `collect`.
#[must_use]
pub fn default_view(role: Option<&RoutedRole>, in_slot: bool) -> PumpView {
    gate::default_view(role, in_slot)
}

#[cfg(test)]
#[path = "pump_test.rs"]
mod tests;
