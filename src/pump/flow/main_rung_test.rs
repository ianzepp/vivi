//! The main rung tick phase: collect finished main rung jobs, then schedule.

use std::collections::BTreeMap;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::{ENV_FAILED, FAILING, record_target};
use super::{
    GREEN, LOST, MAIN_RUNG_FAILED, MAIN_TARGET_UNKNOWN, MainRung, PAYLOAD_KEY, STARTED, TESTED,
    key, last_green,
};
use crate::error::VivariumError;
use crate::pump::config::{PumpConfig, RungsConfig};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::TickPhase;
use crate::pump::flow::mail::MailWriter;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::rung_result::ENV_FAILURE;
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
        self.job_at(status, receipt, TIP, 1)
    }

    /// Add a terminal main rung job for the target at `tip`, attempt `attempt`.
    fn job_at(
        &mut self,
        status: JobStatus,
        receipt: &str,
        tip: &str,
        attempt: u32,
    ) -> Result<String, VivariumError> {
        let mut spec = rung::spec(&self.ctx, TARGET, "main", tip, "main-step", attempt)
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

const TIP2: &str = "fedcba9876543210fedcba9876543210fedcba98";
const TSC_MISSING: &str = "sh: tsc: command not found\nagent exit 127 in 1s\n";

impl Rig {
    fn env_raised(&self) -> usize {
        self.host
            .logs()
            .iter()
            .filter(|line| line.contains(&format!("EXCEPTION {ENV_FAILURE}")))
            .count()
    }
}

#[test]
fn a_missing_tool_is_not_a_red_main() -> R {
    let mut rig = rig(true)?;
    let id = rig.job(JobStatus::Finished, TSC_MISSING)?;
    rig.run()?;
    rig.run()?;
    assert_eq!(rig.env_raised(), 1);
    assert_eq!(rig.raised(), 0);
    assert!(rig.sends().is_empty());
    assert_eq!(rig.store.main_state(&key(TESTED, TARGET))?, None);
    assert_eq!(
        rig.store.main_state(&key(ENV_FAILED, TARGET))?.as_deref(),
        Some(TIP)
    );
    assert_eq!(rig.store.main_state(&key(FAILING, TARGET))?, None);
    assert_eq!(last_green(&rig.store, TARGET)?, None);
    assert!(rig.jobs.jobs[&id].applied_at.is_some());
    Ok(())
}

#[test]
fn the_same_tool_missing_on_a_new_tip_raises_per_tip() -> R {
    let mut rig = rig(true)?;
    rig.job(JobStatus::Finished, TSC_MISSING)?;
    rig.run()?;
    assert_eq!(rig.env_raised(), 1);
    rig.job_at(JobStatus::Finished, TSC_MISSING, TIP2, 1)?;
    rig.run()?;
    assert_eq!(rig.env_raised(), 2);
    assert!(rig.sends().is_empty());
    assert_eq!(rig.store.main_state(&key(TESTED, TARGET))?, None);
    assert_eq!(
        rig.store.main_state(&key(ENV_FAILED, TARGET))?.as_deref(),
        Some(TIP2)
    );
    Ok(())
}

#[test]
fn the_same_tip_failing_again_raises_nothing_new() -> R {
    let mut rig = rig(true)?;
    rig.job(JobStatus::Finished, TSC_MISSING)?;
    rig.run()?;
    rig.job_at(JobStatus::Finished, TSC_MISSING, TIP, 2)?;
    rig.run()?;
    assert_eq!(rig.env_raised(), 1);
    assert_eq!(
        rig.store.main_state(&key(ENV_FAILED, TARGET))?.as_deref(),
        Some(TIP)
    );
    Ok(())
}

#[test]
fn two_lost_jobs_on_one_tip_mark_it_tested() -> R {
    let mut rig = rig(true)?;
    let first = rig.job_at(JobStatus::Lost, "", TIP, 1)?;
    rig.run()?;
    assert_eq!(rig.store.main_state(&key(TESTED, TARGET))?, None);
    assert_eq!(
        rig.store.main_state(&key(LOST, TARGET))?,
        Some(format!("{TIP} 1"))
    );
    // After the interval the schedule re-runs the same tip as attempt 2.
    rig.host.advance(1800);
    rig.run()?;
    let second = format!("rung-main-main-default-{}-a2", &TIP[..12]);
    assert!(first.ends_with("-a1"));
    let job = rig
        .jobs
        .jobs
        .get_mut(&second)
        .ok_or_else(|| VivariumError::Message("no -a2 job scheduled".into()))?;
    assert_eq!(job.status, JobStatus::Pending);
    job.status = JobStatus::Lost;
    job.outcome = Some(JobOutcome {
        state: "finished".into(),
        detail: String::new(),
        receipt: String::new(),
        at: 1,
    });
    rig.run()?;
    assert_eq!(
        rig.store.main_state(&key(TESTED, TARGET))?.as_deref(),
        Some(TIP)
    );
    assert_eq!(rig.raised(), 2);
    assert_eq!(rig.jobs.jobs.len(), 2);
    assert!(rig.sends().is_empty());
    Ok(())
}

#[test]
fn a_malformed_lost_value_is_an_error() -> R {
    let mut rig = rig(true)?;
    rig.store
        .set_main_state(&key(LOST, TARGET), &format!("{TIP} many"))?;
    rig.job_at(JobStatus::Lost, "", TIP, 1)?;
    let Err(VivariumError::Parse(text)) = rig.run() else {
        return Err(VivariumError::Message("expected a Parse error".into()));
    };
    assert!(text.contains("main-lost:default"), "{text}");
    assert!(text.contains("many"), "{text}");
    Ok(())
}

#[test]
fn a_new_tip_resets_the_lost_count() -> R {
    let mut rig = rig(true)?;
    rig.job_at(JobStatus::Lost, "", TIP, 1)?;
    rig.run()?;
    rig.job_at(JobStatus::Lost, "", TIP2, 1)?;
    rig.run()?;
    assert_eq!(rig.store.main_state(&key(TESTED, TARGET))?, None);
    assert_eq!(
        rig.store.main_state(&key(LOST, TARGET))?,
        Some(format!("{TIP2} 1"))
    );
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

#[test]
fn a_finished_job_for_an_unconfigured_target_is_applied_with_one_exception() -> R {
    let mut rig = rig(true)?;
    let id = rig.job(JobStatus::Finished, "ok\nagent exit 0 in 8s")?;
    rig.jobs
        .jobs
        .get_mut(&id)
        .ok_or_else(|| VivariumError::Message("job".into()))?
        .payload[PAYLOAD_KEY] = serde_json::json!("gone");
    rig.run()?;
    rig.run()?;
    assert!(rig.jobs.jobs[&id].applied_at.is_some());
    let lines: Vec<String> = std::fs::read_to_string(rig.ctx.home.join("exceptions.txt"))
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains(MAIN_TARGET_UNKNOWN))
        .map(str::to_owned)
        .collect();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("expected target gone configured with a main rung"));
    assert!(lines[0].contains(&format!("configured [{TARGET}]")));
    assert_eq!(rig.store.main_state(&key(TESTED, TARGET))?, None);
    Ok(())
}
