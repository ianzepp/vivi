//! Main rung scheduling: a new main tip starts one job on the target's task.

use std::collections::BTreeMap;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::{INTERVAL_SECS, schedule};
use crate::error::VivariumError;
use crate::pump::config::{PumpConfig, RungsConfig};
use crate::pump::flow::fake_mail::{Call, FakeMail};
use crate::pump::flow::main_rung::{
    ENV_FAILED, GREEN, LOST, PAYLOAD_KEY, STARTED, TASK, TESTED, key,
};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobStatus, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;

const TARGET: &str = "default";
const TIP_A: &str = "0123456789abcdef0123456789abcdef01234567";
const TIP_B: &str = "fedcba9876543210fedcba9876543210fedcba98";

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
}

fn config(main_rung: bool) -> PumpConfig {
    let mut cfg = PumpConfig::default();
    if main_rung {
        cfg.rungs = RungsConfig {
            main: Some(String::from("cargo test")),
            timeout: Some(900),
            pools: vec![String::from("build-pool")],
            ..RungsConfig::default()
        };
    }
    cfg
}

fn rig(main_rung: bool) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, host) = test_ctx(dir.path(), config(main_rung));
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    Ok(Rig {
        _dir: dir,
        ctx,
        host,
        store,
        mail: FakeMail::new("mind"),
        jobs: Jobs::default(),
    })
}

impl Rig {
    fn main_at(&self, tip: &str) {
        self.host
            .reply("ls-remote", 0, &format!("{tip}\trefs/heads/main\n"));
    }

    fn schedule(&mut self) -> Result<Option<String>, VivariumError> {
        schedule(
            &self.ctx,
            &mut self.jobs,
            &self.store,
            &mut self.mail,
            TARGET,
        )
    }

    fn sends(&self) -> usize {
        self.mail
            .calls
            .iter()
            .filter(|call| matches!(call, Call::SendStep { .. }))
            .count()
    }
}

#[test]
fn no_main_rung_configured_runs_no_command() -> R {
    let mut rig = rig(false)?;
    rig.main_at(TIP_A);
    assert_eq!(rig.schedule()?, None);
    assert!(rig.host.commands().is_empty());
    assert!(rig.mail.calls.is_empty());
    Ok(())
}

#[test]
fn new_tip_enqueues_job_on_preset_task() -> R {
    let mut rig = rig(true)?;
    rig.main_at(TIP_A);
    let id = rig.schedule()?;
    assert_eq!(
        id.as_deref(),
        Some(format!("rung-main-main-default-{}-a1", &TIP_A[..12]).as_str())
    );
    let job = &rig.jobs.jobs[id.as_deref().unwrap_or_default()];
    assert_eq!(job.kind, "rung");
    assert_eq!(job.status, JobStatus::Pending);
    assert_eq!(job.payload[PAYLOAD_KEY], TARGET);
    assert_eq!(job.payload["tip"], TIP_A);
    let task = rig.store.main_state(&key(TASK, TARGET))?;
    assert!(task.is_some());
    assert_eq!(job.task, task);
    assert_eq!(
        rig.store.main_state(&key(STARTED, TARGET))?,
        Some(rig.ctx.now().to_string())
    );
    Ok(())
}

#[test]
fn pending_job_blocks_a_second_start() -> R {
    let mut rig = rig(true)?;
    rig.main_at(TIP_A);
    assert!(rig.schedule()?.is_some());
    rig.host.advance(INTERVAL_SECS + 1);
    rig.main_at(TIP_B);
    assert_eq!(rig.schedule()?, None);
    assert_eq!(rig.jobs.jobs.len(), 1);
    Ok(())
}

#[test]
fn tested_tip_is_not_run_again() -> R {
    let mut rig = rig(true)?;
    rig.main_at(TIP_A);
    rig.store.set_main_state(&key(TESTED, TARGET), TIP_A)?;
    rig.store.set_main_state(&key(GREEN, TARGET), TIP_A)?;
    assert_eq!(rig.schedule()?, None);
    assert!(rig.jobs.jobs.is_empty());
    Ok(())
}

#[test]
fn an_env_failed_tip_is_not_run_again_but_a_new_tip_is() -> R {
    let mut rig = rig(true)?;
    rig.main_at(TIP_A);
    rig.store.set_main_state(&key(ENV_FAILED, TARGET), TIP_A)?;
    assert_eq!(rig.schedule()?, None);
    assert!(rig.jobs.jobs.is_empty());
    rig.main_at(TIP_B);
    let id = rig.schedule()?;
    assert_eq!(
        id.as_deref(),
        Some(format!("rung-main-main-default-{}-a1", &TIP_B[..12]).as_str())
    );
    Ok(())
}

#[test]
fn a_new_tip_after_an_env_failure_waits_for_the_interval() -> R {
    let mut rig = rig(true)?;
    rig.main_at(TIP_A);
    rig.schedule()?;
    for job in rig.jobs.jobs.values_mut() {
        job.status = JobStatus::Finished;
    }
    rig.store.set_main_state(&key(ENV_FAILED, TARGET), TIP_A)?;
    rig.main_at(TIP_B);
    rig.host.advance(INTERVAL_SECS - 1);
    assert_eq!(rig.schedule()?, None);
    rig.host.advance(1);
    assert!(rig.schedule()?.is_some());
    Ok(())
}

#[test]
fn new_tip_waits_for_the_interval() -> R {
    let mut rig = rig(true)?;
    rig.main_at(TIP_A);
    let first = rig.schedule()?.unwrap_or_default();
    for job in rig.jobs.jobs.values_mut() {
        job.status = JobStatus::Finished;
    }
    rig.store.set_main_state(&key(TESTED, TARGET), TIP_A)?;
    rig.main_at(TIP_B);
    rig.host.advance(INTERVAL_SECS - 1);
    assert_eq!(rig.schedule()?, None);
    rig.host.advance(1);
    let second = rig.schedule()?;
    assert_eq!(
        second.as_deref(),
        Some(format!("rung-main-main-default-{}-a1", &TIP_B[..12]).as_str())
    );
    assert_ne!(second.unwrap_or_default(), first);
    Ok(())
}

#[test]
fn task_is_sent_once_and_reused() -> R {
    let mut rig = rig(true)?;
    rig.main_at(TIP_A);
    rig.schedule()?;
    let task = rig.store.main_state(&key(TASK, TARGET))?;
    for job in rig.jobs.jobs.values_mut() {
        job.status = JobStatus::Finished;
    }
    rig.store.set_main_state(&key(TESTED, TARGET), TIP_A)?;
    rig.main_at(TIP_B);
    rig.host.advance(INTERVAL_SECS);
    rig.schedule()?;
    assert_eq!(rig.sends(), 1);
    assert_eq!(rig.store.main_state(&key(TASK, TARGET))?, task);
    let tasks: Vec<_> = rig.jobs.jobs.values().map(|job| job.task.clone()).collect();
    assert_eq!(tasks, vec![task.clone(), task]);
    Ok(())
}

#[test]
fn a_stored_lost_count_makes_the_next_job_attempt_two() -> R {
    let mut rig = rig(true)?;
    rig.main_at(TIP_A);
    rig.store
        .set_main_state(&key(LOST, TARGET), &format!("{TIP_A} 1"))?;
    let id = rig.schedule()?;
    assert_eq!(
        id.as_deref(),
        Some(format!("rung-main-main-default-{}-a2", &TIP_A[..12]).as_str())
    );
    Ok(())
}

#[test]
fn a_lost_count_for_another_tip_leaves_attempt_one() -> R {
    let mut rig = rig(true)?;
    rig.main_at(TIP_A);
    rig.store
        .set_main_state(&key(LOST, TARGET), &format!("{TIP_B} 1"))?;
    let id = rig.schedule()?;
    assert_eq!(
        id.as_deref(),
        Some(format!("rung-main-main-default-{}-a1", &TIP_A[..12]).as_str())
    );
    Ok(())
}

#[test]
fn a_job_id_that_already_exists_is_an_error() -> R {
    let mut rig = rig(true)?;
    rig.main_at(TIP_A);
    let id = rig.schedule()?.unwrap_or_default();
    if let Some(job) = rig.jobs.jobs.get_mut(&id) {
        job.status = JobStatus::Lost;
    }
    rig.host.advance(INTERVAL_SECS);
    // No lost count was recorded, so the next attempt id collides.
    let Err(VivariumError::Other(text)) = rig.schedule() else {
        return Err(VivariumError::Message("expected an error".into()));
    };
    assert!(text.contains(&id), "{text}");
    Ok(())
}
