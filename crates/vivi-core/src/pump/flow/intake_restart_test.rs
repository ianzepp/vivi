//! Tests that a build parked `restarting` with a saved entry competes with the
//! fresh needs in the same ranking, that its parent takes no cap slot, and that
//! a committed successor is counted once.

use std::collections::BTreeMap;

use tempfile::{TempDir, tempdir};

use super::register;
use crate::pump::config::PumpConfig;
use crate::pump::flow::advance::start::start;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{FLOW_VERSION_NUMBER, RunRow, run_state};
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::tick::tick_with;
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::jobs::{JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};
use vivi_error::VivariumError;
use vivi_mail::letter::fixture::record_body;

type R = Result<(), VivariumError>;

const PARKED: &str = "parked";

#[derive(Default)]
struct Jobs {
    jobs: BTreeMap<String, PoolJob>,
}

impl JobStore for Jobs {
    fn jobs(&self) -> &BTreeMap<String, PoolJob> {
        &self.jobs
    }

    fn jobs_mut(&mut self) -> &mut BTreeMap<String, PoolJob> {
        &mut self.jobs
    }
}

struct Rig {
    _dir: TempDir,
    ctx: Ctx,
    store: PumpStore,
    mail: FakeMail,
    jobs: Jobs,
    registry: Registry<Jobs, FakeMail>,
}

fn rig(cap: Option<u32>) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let mut cfg = PumpConfig::default();
    cfg.flow.max_live_runs = cap;
    let (ctx, _host) = test_ctx(dir.path(), cfg);
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut registry = Registry::new();
    register(&mut registry);
    Ok(Rig {
        _dir: dir,
        ctx,
        store,
        mail: FakeMail::new("pump"),
        jobs: Jobs::default(),
        registry,
    })
}

impl Rig {
    fn open(&mut self, need: &str) -> R {
        MailWriter::open_record(&mut self.mail, need, &record_body(need, "build")).map(|_| ())
    }

    fn priority(&self, need: &str, level: i64) -> R {
        self.store.set_need_priority(need, level, 1)
    }

    /// A live run 1 of `need`, left `restarting` at `entry` with no successor.
    fn park(&mut self, need: &str, entry: &str) -> R {
        self.open(need)?;
        start(&self.ctx, &self.store, &mut self.mail, need, None)?;
        self.store.set_restart_entry(need, 1, entry)?;
        self.store.set_run_state(need, 1, run_state::RESTARTING)
    }

    fn tick(&mut self) -> R {
        tick_with(
            &self.ctx,
            &mut self.jobs,
            &self.store,
            &mut self.mail,
            &self.registry,
        )
    }

    fn runs(&self, need: &str) -> Result<Vec<(u32, String, String)>, VivariumError> {
        Ok(self
            .store
            .runs_of(need)?
            .into_iter()
            .map(|r| (r.run, r.entry, r.state))
            .collect())
    }
}

#[test]
fn a_parked_restart_ranks_with_the_fresh_needs_and_its_parent_takes_no_slot() -> R {
    let mut rig = rig(Some(2))?;
    rig.park(PARKED, "rework1")?;
    rig.priority(PARKED, 5)?;
    for (need, level) in [("ready-a", 2), ("ready-b", 1)] {
        rig.open(need)?;
        rig.priority(need, level)?;
    }

    rig.tick()?;

    // The parked restart outranked both fresh needs and started at its saved
    // entry; the second slot went to the better fresh need, so the parent took
    // no slot of its own.
    assert_eq!(
        rig.runs(PARKED)?,
        [
            (1, "plan".into(), "superseded".into()),
            (2, "rework1".into(), "live".into())
        ]
    );
    assert_eq!(rig.runs("ready-a")?.len(), 1);
    assert!(rig.runs("ready-b")?.is_empty(), "the cap holds at two");
    assert!(rig.store.build_waits()?.is_empty());
    Ok(())
}

#[test]
fn a_lower_ranked_parked_restart_waits_behind_a_fresh_need() -> R {
    let mut rig = rig(Some(1))?;
    rig.park(PARKED, "rework1")?;
    rig.priority(PARKED, 1)?;
    rig.open("ready")?;
    rig.priority("ready", 9)?;

    rig.tick()?;

    assert_eq!(rig.runs("ready")?.len(), 1, "the fresh need took the slot");
    assert_eq!(
        rig.runs(PARKED)?,
        [(1, "plan".into(), "restarting".into())],
        "the parked restart stays queued with its intent"
    );
    assert_eq!(
        rig.store.restart_entry(PARKED, 1)?.as_deref(),
        Some("rework1")
    );

    // A reopened store still knows the intent.
    let reopened = PumpStore::open(&rig._dir.path().join(DB_FILENAME))?;
    assert_eq!(
        reopened.restart_entry(PARKED, 1)?.as_deref(),
        Some("rework1")
    );
    Ok(())
}

#[test]
fn a_committed_successor_is_counted_once_not_with_its_parent() -> R {
    let mut rig = rig(Some(1))?;
    rig.park(PARKED, "rework1")?;
    rig.store.insert_run(&RunRow {
        need: PARKED.into(),
        run: 2,
        flow: "build".into(),
        flow_version: FLOW_VERSION_NUMBER,
        kind: "build".into(),
        entry: "rework1".into(),
        state: run_state::LIVE.into(),
    })?;
    rig.open("ready")?;

    rig.tick()?;

    assert!(
        rig.runs("ready")?.is_empty(),
        "parent plus successor is one run"
    );
    assert_eq!(rig.runs(PARKED)?.len(), 2);
    Ok(())
}

#[test]
fn a_parked_restart_without_a_saved_entry_starts_nothing() -> R {
    let mut rig = rig(None)?;
    rig.open(PARKED)?;
    start(&rig.ctx, &rig.store, &mut rig.mail, PARKED, None)?;
    rig.store.set_run_state(PARKED, 1, run_state::RESTARTING)?;

    rig.tick()?;

    assert_eq!(
        rig.runs(PARKED)?,
        [(1, "plan".into(), "restarting".into())],
        "a missing entry is not a queue position"
    );
    Ok(())
}
