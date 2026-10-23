//! The main rung tick phase: collect finished main rung jobs, then schedule.

use std::collections::BTreeMap;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::{FAILING, record_target};
use super::{GREEN, MAIN_RUNG_FAILED, MainRung, PAYLOAD_KEY, STARTED, TESTED, key, last_green};
use crate::error::VivariumError;
use crate::pump::config::{PumpConfig, RungsConfig};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::TickPhase;
use crate::pump::flow::mail::MailWriter;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobOutcome, JobStatus, JobStore, PoolJob, rung};
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;

const TARGET: &str = "default";
const TIP: &str = "0123456789abcdef0123456789abcdef01234567";

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

fn rig(main_rung: bool) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let mut cfg = PumpConfig::default();
    if main_rung {
        cfg.rungs = RungsConfig {
            main: Some(String::from("cargo test")),
            timeout: Some(900),
            pools: vec![String::from("build-pool")],
            ..RungsConfig::default()
        };
    }
    let (ctx, host) = test_ctx(dir.path(), cfg);
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    // Main sits at the tested tip, and the interval blocks a fresh start.
    host.reply("ls-remote", 0, &format!("{TIP}\trefs/heads/main\n"));
    store.set_main_state(&key(STARTED, TARGET), &ctx.now().to_string())?;
    Ok(Rig {
        _dir: dir,
        ctx,
        host,
        store,
        mail: FakeMail::new("pump"),
        jobs: Jobs::default(),
    })
}

impl Rig {
    /// Add a terminal main rung job for the target at `TIP`.
    fn job(&mut self, status: JobStatus, receipt: &str) -> Result<String, VivariumError> {
        let mut spec = rung::spec(&self.ctx, TARGET, "main", TIP, "main-step", 1)
            .ok_or_else(|| VivariumError::Message("no main rung".into()))?;
        spec.payload[PAYLOAD_KEY] = serde_json::json!(TARGET);
        let id = spec.id.clone();
        let mut job = PoolJob::from(spec);
        job.status = status;
        job.outcome = Some(JobOutcome {
            state: "finished".into(),
            detail: String::new(),
            receipt: receipt.to_owned(),
            at: 1,
        });
        self.jobs.jobs.insert(id.clone(), job);
        Ok(id)
    }

    fn run(&mut self) -> R {
        TickPhase::run(
            &MainRung,
            &self.ctx,
            &mut self.jobs,
            &self.store,
            &mut self.mail,
        )
    }

    fn sends(&self) -> Vec<String> {
        self.host
            .commands()
            .into_iter()
            .filter(|c| c.contains("need send"))
            .collect()
    }

    fn raised(&self) -> usize {
        self.host
            .logs()
            .iter()
            .filter(|line| line.contains(&format!("EXCEPTION {MAIN_RUNG_FAILED}")))
            .count()
    }
}

#[test]
fn a_passing_job_sets_last_green_and_tested_and_clears_failing() -> R {
    let mut rig = rig(true)?;
    rig.store
        .set_main_state(&key(FAILING, TARGET), "[\"a::t\"]")?;
    let id = rig.job(JobStatus::Finished, "ok\nagent exit 0 in 8s")?;
    rig.run()?;
    assert_eq!(last_green(&rig.store, TARGET)?.as_deref(), Some(TIP));
    assert_eq!(
        rig.store.main_state(&key(TESTED, TARGET))?.as_deref(),
        Some(TIP)
    );
    assert_eq!(rig.store.main_state(&key(FAILING, TARGET))?, None);
    assert!(rig.jobs.jobs[&id].applied_at.is_some());
    assert!(rig.sends().is_empty());
    Ok(())
}

#[test]
fn a_failing_job_files_a_repair_need_and_records_the_failing_set() -> R {
    let mut rig = rig(true)?;
    let need = rig.mail.send_step("pump", "pump", "repair", "need", &[])?;
    rig.host
        .reply("need send", 0, &format!("created need {need}\n"));
    let receipt = "     Running unittests src/lib.rs (target/debug/deps/mir_emit_harness-1a2b3c4d5e)\n\
                   test emit::a ... FAILED\n\
                   agent exit 101 in 9s\n";
    rig.job(JobStatus::Finished, receipt)?;
    rig.run()?;
    let sends = rig.sends();
    assert_eq!(sends.len(), 1);
    assert!(sends[0].contains("--mark repair"), "{}", sends[0]);
    assert_eq!(
        rig.store.main_state(&key(FAILING, TARGET))?.as_deref(),
        Some("[\"mir_emit_harness::emit::a\"]")
    );
    assert_eq!(
        rig.store.main_state(&key(TESTED, TARGET))?.as_deref(),
        Some(TIP)
    );
    assert_eq!(last_green(&rig.store, TARGET)?, None);
    Ok(())
}

#[test]
fn a_lost_job_raises_once_and_sets_nothing() -> R {
    let mut rig = rig(true)?;
    let id = rig.job(JobStatus::Lost, "")?;
    rig.run()?;
    rig.run()?;
    assert_eq!(rig.raised(), 1);
    assert_eq!(rig.store.main_state(&key(TESTED, TARGET))?, None);
    assert_eq!(rig.store.main_state(&key(GREEN, TARGET))?, None);
    assert!(rig.jobs.jobs[&id].applied_at.is_some());
    Ok(())
}

#[test]
fn no_main_rung_configured_runs_no_command() -> R {
    let mut rig = rig(false)?;
    rig.run()?;
    assert!(rig.host.commands().is_empty());
    assert!(rig.mail.calls.is_empty());
    assert!(rig.jobs.jobs.is_empty());
    Ok(())
}

#[test]
fn record_target_reads_the_target_line_and_ignores_a_missing_one() {
    assert_eq!(record_target("kind: repair\ntarget: x\n"), Some("x"));
    assert_eq!(record_target("kind: repair\n"), None);
    assert_eq!(record_target("target:  \n"), None);
}
