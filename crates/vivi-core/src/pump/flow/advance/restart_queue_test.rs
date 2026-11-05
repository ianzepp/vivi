//! Tests that a build restart with no successor is left to intake's ranked
//! queue: capacity and prerequisite waiting raise no stuck alarm, a missing
//! entry stays detectable, and an already-created successor still recovers.

use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tempfile::{TempDir, tempdir};

use super::{RESTART_STUCK, RestartResult, register, resume};
use crate::pump::config::PumpConfig;
use crate::pump::flow::advance::start::start;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{FLOW_VERSION_NUMBER, RunRow, run_state};
use crate::pump::flow::intake::register as register_intake;
use crate::pump::flow::mail::{MailReader, MailWriter};
use crate::pump::flow::tick::tick_with;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};
use vivi_error::VivariumError;

type R = Result<(), VivariumError>;

const NEED: &str = "n1";
const OTHER: &str = "n2";

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
    host: Arc<FakeHost>,
    store: PumpStore,
    mail: FakeMail,
    jobs: Jobs,
    registry: Registry<Jobs, FakeMail>,
}

/// A pump home with the two tick phases that finish a restart, under `cap`.
fn rig(cap: Option<u32>) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let mut cfg = PumpConfig::default();
    cfg.flow.max_live_runs = cap;
    let (ctx, host) = test_ctx(dir.path(), cfg);
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mail = FakeMail::new("pump");
    let mut registry = Registry::new();
    register(&mut registry);
    register_intake(&mut registry);
    Ok(Rig {
        _dir: dir,
        ctx,
        host,
        store,
        mail,
        jobs: Jobs::default(),
        registry,
    })
}

impl Rig {
    fn tick(&mut self) -> R {
        tick_with(
            &self.ctx,
            &mut self.jobs,
            &self.store,
            &mut self.mail,
            &self.registry,
        )
    }

    /// A live run 1 of `need`, holding a cap slot.
    fn live(&mut self, need: &str) -> R {
        MailWriter::open_record(&mut self.mail, need, "record")?;
        start(&self.ctx, &self.store, &mut self.mail, need, None).map(|_| ())
    }

    /// A live run 1 of `need`, left `restarting` at `entry` when one is given.
    fn park(&mut self, need: &str, entry: Option<&str>) -> R {
        MailWriter::open_record(&mut self.mail, need, "record")?;
        let old = start(&self.ctx, &self.store, &mut self.mail, need, None)?;
        if let Some(entry) = entry {
            self.store.set_restart_entry(need, old.run, entry)?;
        }
        self.store
            .set_run_state(need, old.run, run_state::RESTARTING)
    }

    fn cap(&mut self, runs: Option<u32>) {
        Arc::make_mut(&mut self.ctx.cfg).flow.max_live_runs = runs;
    }

    fn runs(&self) -> Result<Vec<(u32, String, String)>, VivariumError> {
        Ok(self
            .store
            .runs_of(NEED)?
            .into_iter()
            .map(|r| (r.run, r.entry, r.state))
            .collect())
    }

    fn exceptions(&self) -> String {
        fs::read_to_string(self.ctx.home.join("exceptions.txt")).unwrap_or_default()
    }
}

fn epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[test]
fn a_parked_build_restart_starts_through_intake_at_its_saved_entry() -> R {
    let mut rig = rig(None)?;
    rig.park(NEED, Some("rework1"))?;
    rig.tick()?;

    assert_eq!(
        rig.runs()?,
        [
            (1, "plan".into(), "superseded".into()),
            (2, "rework1".into(), "live".into())
        ],
        "exactly one successor, started at the saved entry"
    );
    assert!(
        rig.store.build_waits()?.is_empty(),
        "the start clears the wait projection"
    );
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn a_parked_build_restart_waits_for_capacity_without_an_alarm() -> R {
    let mut rig = rig(None)?;
    rig.live(OTHER)?;
    rig.cap(Some(1));
    rig.park(NEED, Some("plan"))?;
    rig.host.set_now(epoch());
    rig.tick()?;
    assert_eq!(
        rig.runs()?.len(),
        1,
        "the full cap leaves the restart queued"
    );

    // Past the stuck line the queued restart is still waiting, not stuck.
    rig.host.set_now(epoch() + 700);
    rig.tick()?;
    rig.tick()?;
    assert_eq!(
        rig.exceptions().matches(RESTART_STUCK).count(),
        0,
        "capacity waiting is not a failed restart: {}",
        rig.exceptions()
    );
    assert_eq!(rig.runs()?, [(1, "plan".into(), "restarting".into())]);
    assert_eq!(
        rig.store.restart_entry(NEED, 1)?.as_deref(),
        Some("plan"),
        "the requested entry stays durable"
    );
    Ok(())
}

#[test]
fn a_parked_build_restart_waits_for_a_prerequisite_without_an_alarm() -> R {
    let mut rig = rig(None)?;
    rig.park(NEED, Some("rework1"))?;
    rig.mail.set_prerequisites(NEED, &["dep"]);
    rig.host.set_now(epoch());
    rig.tick()?;
    rig.host.set_now(epoch() + 700);
    rig.tick()?;

    assert_eq!(rig.runs()?, [(1, "plan".into(), "restarting".into())]);
    assert_eq!(rig.store.build_waits()?.len(), 1, "the blocker is queued");
    assert_eq!(
        rig.exceptions().matches(RESTART_STUCK).count(),
        0,
        "prerequisite waiting is not a failed restart: {}",
        rig.exceptions()
    );

    // The prerequisite lands: the successor starts with no second action.
    rig.mail.set_prerequisites(NEED, &[]);
    rig.tick()?;
    assert_eq!(
        rig.runs()?,
        [
            (1, "plan".into(), "superseded".into()),
            (2, "rework1".into(), "live".into())
        ]
    );
    assert!(rig.store.build_waits()?.is_empty());
    Ok(())
}

#[test]
fn a_build_restart_with_no_entry_stays_detectable() -> R {
    let mut rig = rig(None)?;
    rig.park(NEED, None)?;
    rig.host.set_now(epoch());
    rig.tick()?;
    rig.host.set_now(epoch() + 700);
    rig.tick()?;

    assert_eq!(rig.runs()?, [(1, "plan".into(), "restarting".into())]);
    assert_eq!(
        rig.exceptions().matches(RESTART_STUCK).count(),
        1,
        "a restart with no saved entry is corruption, not a queue position: {}",
        rig.exceptions()
    );
    Ok(())
}

#[test]
fn an_already_created_successor_recovers_even_with_the_cap_full() -> R {
    let mut rig = rig(Some(1))?;
    rig.park(NEED, Some("rework1"))?;
    rig.live(OTHER)?;
    let old = rig.store.runs_of(NEED)?.remove(0);
    rig.store.insert_run(&RunRow {
        run: old.run + 1,
        entry: "rework1".into(),
        state: run_state::STARTING.to_string(),
        ..old.clone()
    })?;
    rig.mail.set_prerequisites(NEED, &["dep"]);

    resume(&rig.ctx, &rig.store, &mut rig.mail, &old)?;
    assert_eq!(
        rig.runs()?,
        [
            (1, "plan".into(), "superseded".into()),
            (2, "rework1".into(), "live".into())
        ],
        "a committed successor is recovered unconditionally"
    );
    assert!(
        rig.store.build_waits()?.is_empty(),
        "recovery is not re-gated"
    );
    Ok(())
}

#[test]
fn an_immediate_restart_still_replaces_its_occupied_slot() -> R {
    let mut rig = rig(None)?;
    rig.park(NEED, Some("plan"))?;
    let old = rig.store.runs_of(NEED)?.remove(0);
    let started = super::restart(&rig.ctx, &rig.store, &mut rig.mail, &old, "rework1")?;
    let RestartResult::Started(next) = started else {
        panic!("an admitted restart starts its successor");
    };
    assert_eq!((next.run, next.state.as_str()), (2, "live"));
    assert_eq!(
        rig.runs()?,
        [
            (1, "plan".into(), "superseded".into()),
            (2, "rework1".into(), "live".into())
        ]
    );
    Ok(())
}

/// A parked build with a queued wait, as a blocked prerequisite leaves it.
fn parked_with_wait(rig: &mut Rig) -> R {
    rig.park(NEED, Some("rework1"))?;
    rig.mail.set_prerequisites(NEED, &["dep"]);
    rig.tick()?;
    assert_eq!(rig.store.build_waits()?.len(), 1, "the wait is queued");
    Ok(())
}

#[test]
fn a_need_that_closes_withdraws_its_deferred_parent() -> R {
    let mut rig = rig(None)?;
    parked_with_wait(&mut rig)?;
    rig.mail.close_need(NEED, "done")?;

    rig.tick()?;

    assert_eq!(
        rig.runs()?,
        [(1, "plan".into(), "withdrawn".into())],
        "a closed need withdraws the waiting parent"
    );
    assert!(
        rig.store.build_waits()?.is_empty(),
        "withdrawal drops the stale wait projection"
    );
    let sentinel = rig
        .store
        .steps_of(NEED, 1)?
        .into_iter()
        .find(|row| row.node == "sentinel")
        .expect("sentinel row");
    assert_eq!(
        rig.mail.task_state(&sentinel.task)?.verdict.as_deref(),
        Some("withdrawn")
    );
    assert_eq!(rig.mail.record_outcome(NEED), Some("withdrawn"));

    rig.tick()?;
    assert_eq!(
        rig.runs()?,
        [(1, "plan".into(), "withdrawn".into())],
        "a withdrawn parent never starts a successor"
    );
    Ok(())
}

#[test]
fn a_record_that_closes_withdraws_its_deferred_parent() -> R {
    let mut rig = rig(None)?;
    parked_with_wait(&mut rig)?;
    MailWriter::close_record(&mut rig.mail, NEED, "withdrawn")?;

    rig.tick()?;

    assert_eq!(
        rig.runs()?,
        [(1, "plan".into(), "withdrawn".into())],
        "a closed record withdraws the waiting parent"
    );
    assert!(rig.store.build_waits()?.is_empty());
    let sentinel = rig
        .store
        .steps_of(NEED, 1)?
        .into_iter()
        .find(|row| row.node == "sentinel")
        .expect("sentinel row");
    assert_eq!(
        rig.mail.task_state(&sentinel.task)?.verdict.as_deref(),
        Some("withdrawn")
    );

    rig.mail.set_prerequisites(NEED, &[]);
    rig.tick()?;
    assert_eq!(
        rig.runs()?,
        [(1, "plan".into(), "withdrawn".into())],
        "a landed prerequisite cannot revive a withdrawn parent"
    );
    Ok(())
}
