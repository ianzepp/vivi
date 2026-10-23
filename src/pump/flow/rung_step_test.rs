//! Rung steps: start at the need-branch tip, retry once on lost or timeout,
//! answer from the exit status with machine-read failing ids.

use std::collections::BTreeMap;

use tempfile::{TempDir, tempdir};

use super::{answer, job, register};
use crate::error::VivariumError;
use crate::letter::fixture::step_body;
use crate::pump::config::{PumpConfig, RungsConfig};
use crate::pump::flow::advance::start::start;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::tick::tick_with;
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::jobs::{JobOutcome, JobStatus, JobStore, PoolJob};
use crate::pump::store::step_payload::StepPayloadRow;
use crate::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};

type R = Result<(), VivariumError>;
type Reg = Registry<Jobs, FakeMail>;

const NEED: &str = "n1";
const BASE: &str = "0123456789abcdef0123456789abcdef01234567";
const STEP_NODE: &str = "test1";

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
    registry: Reg,
    node: &'static str,
}

fn config(rungs: bool) -> PumpConfig {
    let mut cfg = PumpConfig::default();
    if rungs {
        cfg.rungs = RungsConfig {
            need: Some(String::from("cargo test")),
            main: Some(String::from("cargo test --all")),
            timeout: Some(900),
            pools: vec![String::from("build-pool")],
            ..RungsConfig::default()
        };
    }
    cfg
}

/// A build run with a pending `test1` (need-rung) step and a live need branch.
fn rig(cfg: PumpConfig) -> Result<Rig, VivariumError> {
    rig_at(cfg, STEP_NODE)
}

/// As [`rig`], with the pending rung step on `node`.
fn rig_at(cfg: PumpConfig, node: &'static str) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, _host) = test_ctx(dir.path(), cfg);
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("mind");
    MailWriter::open_record(&mut mail, NEED, "kind: build")?;
    start(&ctx, &store, &mut mail, NEED, Some("plan"))?;
    let task = mail.send_step("mind", "mind", "rung", &step_body(NEED, 1, node, ""), &[])?;
    store.insert_step(
        NEED,
        &StepRow {
            run: 1,
            node: node.into(),
            task,
            state: StepState::Pending,
            verdict: None,
        },
    )?;
    store.upsert_need_branch(&NeedBranchRow {
        need: NEED.into(),
        branch: format!("vivi/need/{NEED}"),
        base: Some(BASE.into()),
        status: "live".into(),
        run: 1,
        target: "default".into(),
        priors: Vec::new(),
    })?;
    let mut registry = Reg::new();
    register(&mut registry);
    Ok(Rig {
        _dir: dir,
        ctx,
        store,
        mail,
        jobs: Jobs::default(),
        registry,
        node,
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

    fn step(&self) -> Result<StepRow, VivariumError> {
        self.store
            .steps_of(NEED, 1)?
            .into_iter()
            .find(|row| row.node == self.node)
            .ok_or_else(|| VivariumError::Message("no rung step".into()))
    }

    /// Mark a job terminal with a receipt.
    fn finish(&mut self, id: &str, status: JobStatus, receipt: &str) -> R {
        let job = self
            .jobs
            .jobs
            .get_mut(id)
            .ok_or_else(|| VivariumError::Message(format!("no job {id}")))?;
        job.status = status;
        job.outcome = Some(JobOutcome {
            state: "finished".into(),
            detail: String::new(),
            receipt: receipt.to_owned(),
            at: 1,
        });
        Ok(())
    }

    fn job_ids(&self) -> Vec<String> {
        self.jobs.jobs.keys().cloned().collect()
    }
}

fn first_job(rig: &mut Rig) -> Result<String, VivariumError> {
    rig.tick()?;
    let task = rig.step()?.task;
    assert_eq!(rig.job_ids(), vec![format!("rung-need-{task}-a1")]);
    Ok(format!("rung-need-{task}-a1"))
}

#[test]
fn a_rung_need_step_enqueues_its_job_at_the_need_branch_tip() -> R {
    let mut rig = rig(config(true))?;
    let id = first_job(&mut rig)?;
    let queued = &rig.jobs.jobs[&id];
    assert_eq!(queued.kind, "rung");
    assert_eq!(queued.from_ref.as_deref(), Some(BASE));
    assert_eq!(queued.target.as_deref(), Some("default"));
    assert_eq!(queued.task.as_deref(), Some(rig.step()?.task.as_str()));
    assert_eq!(queued.command.as_deref(), Some("cargo test"));
    assert_eq!(rig.step()?.state, StepState::Running);
    Ok(())
}

#[test]
fn a_lost_job_runs_again_then_answers_failed() -> R {
    let mut rig = rig(config(true))?;
    let a1 = first_job(&mut rig)?;
    rig.finish(&a1, JobStatus::Lost, "")?;
    rig.tick()?;
    let task = rig.step()?.task;
    let a2 = format!("rung-need-{task}-a2");
    assert!(rig.job_ids().contains(&a2), "{:?}", rig.job_ids());
    assert_eq!(rig.step()?.state, StepState::Running);
    let again = &rig.jobs.jobs[&a2];
    assert_eq!(again.from_ref.as_deref(), Some(BASE));
    assert_eq!(again.payload["attempt"], serde_json::json!(2));
    assert_eq!(rig.store.last_attempt(&task)?, 2);
    let stored = rig
        .store
        .step_job(&a2)?
        .ok_or_else(|| VivariumError::Message("no step_jobs row for a2".into()))?;
    let old = rig
        .store
        .step_job(&a1)?
        .ok_or_else(|| VivariumError::Message("no step_jobs row for a1".into()))?;
    assert_eq!(stored.attempt, old.attempt + 1);
    assert_eq!(again.payload["attempt"], serde_json::json!(stored.attempt));
    assert_eq!(again.payload["run"], serde_json::json!(stored.run));
    assert_eq!(again.payload["node"], serde_json::json!(stored.node));
    assert_eq!(again.payload["task"], serde_json::json!(stored.task));
    assert_eq!(again.task.as_deref(), Some(stored.task.as_str()));

    rig.finish(&a2, JobStatus::Timeout, "")?;
    rig.tick()?;
    assert_eq!(rig.job_ids().len(), 2);
    let step = rig.step()?;
    assert_eq!(step.state, StepState::Done);
    assert_eq!(step.verdict.as_deref(), Some("failed"));
    Ok(())
}

#[test]
fn exit_zero_answers_pass_and_exit_one_answers_fail() -> R {
    let mut pass = rig(config(true))?;
    let id = first_job(&mut pass)?;
    pass.finish(&id, JobStatus::Finished, "ok\nagent exit 0 in 8s")?;
    pass.tick()?;
    assert_eq!(pass.step()?.verdict.as_deref(), Some("pass"));

    let mut fail = rig(config(true))?;
    let id = first_job(&mut fail)?;
    fail.finish(&id, JobStatus::Finished, "boom\nagent exit 1 in 8s")?;
    fail.tick()?;
    assert_eq!(fail.step()?.verdict.as_deref(), Some("fail"));
    assert_eq!(fail.job_ids().len(), 1);
    Ok(())
}

fn finished(receipt: &str) -> PoolJob {
    let mut spec = crate::pump::jobs::JobSpec::new("rung-need-t-a1", "rung", "pump-rung", "s", "b");
    spec.read_only = true;
    let mut job = PoolJob::from(spec);
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: "finished".into(),
        detail: String::new(),
        receipt: receipt.to_owned(),
        at: 1,
    });
    job
}

#[test]
fn a_failed_rung_payload_lists_the_failing_ids_from_the_receipt() {
    let receipt = "     Running unittests src/lib.rs (target/debug/deps/mir_emit_harness-1a2b3c4d5e)\n\
                   test emit::b ... FAILED\n\
                   test emit::a ... FAILED\n\
                   agent exit 101 in 9s\n";
    let (word, payload) = answer(&finished(receipt));
    assert_eq!(word, "fail");
    assert_eq!(payload["exit"], serde_json::json!(101));
    assert_eq!(
        payload["failing"],
        serde_json::json!(["mir_emit_harness::emit::a", "mir_emit_harness::emit::b"])
    );
    assert!(payload["tail"].as_str().unwrap().contains("emit::a"));
}

#[test]
fn a_passing_rung_payload_has_no_failing_ids() {
    let (word, payload) = answer(&finished("ok\nagent exit 0 in 8s"));
    assert_eq!(word, "pass");
    assert_eq!(payload["failing"], serde_json::json!([]));
}

#[test]
fn an_unconfigured_rung_is_an_error_naming_the_rung_and_target() -> R {
    let rig = rig(config(false))?;
    let step = rig.step()?;
    let err = job(&rig.ctx, "default", "need", BASE, &step, 1).unwrap_err();
    let text = err.to_string();
    assert!(text.contains("need") && text.contains("default"), "{text}");
    Ok(())
}

#[test]
fn job_binds_the_step_task_and_attempt() -> R {
    let rig = rig(config(true))?;
    let step = rig.step()?;
    let spec = job(&rig.ctx, "default", "need", BASE, &step, 2)?;
    assert_eq!(spec.task.as_deref(), Some(step.task.as_str()));
    assert_eq!(spec.id, format!("rung-need-{}-a2", step.task));
    Ok(())
}

#[test]
fn a_step_with_no_need_branch_tip_holds_an_error_on_the_run() -> R {
    let mut rig = rig(config(true))?;
    rig.store.set_need_branch_status(NEED, "pruned")?;
    rig.tick()?;
    assert!(rig.jobs.jobs.is_empty());
    assert_eq!(rig.step()?.state, StepState::Pending);
    Ok(())
}

/// Store an auditor payload for the run, naming `raise_rung` when given.
fn audit_payload(rig: &Rig, raise: Option<&str>) -> R {
    let front = raise.map_or_else(
        || serde_json::json!({}),
        |rung| serde_json::json!({ "raise_rung": rung }),
    );
    rig.store.record_step_payload(&StepPayloadRow {
        need: NEED.into(),
        run: 1,
        node: "audit1".into(),
        class: "auditor".into(),
        task: "t-audit".into(),
        payload: serde_json::json!({ "front": front, "body": "" }),
    })
}

#[test]
fn a_raised_step_runs_the_rung_the_audit_named() -> R {
    let mut rig = rig_at(config(true), "raise1")?;
    audit_payload(&rig, Some("main"))?;
    rig.tick()?;
    let task = rig.step()?.task;
    let id = format!("rung-main-{task}-a1");
    assert_eq!(rig.job_ids(), vec![id.clone()]);
    assert_eq!(
        rig.jobs.jobs[&id].command.as_deref(),
        Some("cargo test --all")
    );
    Ok(())
}

#[test]
fn a_raised_step_without_a_named_rung_runs_the_need_rung() -> R {
    let mut bare = rig_at(config(true), "raise1")?;
    bare.tick()?;
    let task = bare.step()?.task;
    assert_eq!(bare.job_ids(), vec![format!("rung-need-{task}-a1")]);

    let mut none = rig_at(config(true), "raise1")?;
    audit_payload(&none, None)?;
    none.tick()?;
    let task = none.step()?.task;
    assert_eq!(none.job_ids(), vec![format!("rung-need-{task}-a1")]);
    Ok(())
}

#[test]
fn a_raised_step_picks_the_older_raise_when_the_newest_audit_has_none() -> R {
    let mut rig = rig_at(config(true), "raise1")?;
    audit_payload(&rig, Some("main"))?;
    rig.store.record_step_payload(&StepPayloadRow {
        need: NEED.into(),
        run: 1,
        node: "audit2".into(),
        class: "auditor".into(),
        task: "t-audit2".into(),
        payload: serde_json::json!({ "front": {}, "body": "" }),
    })?;
    rig.tick()?;
    let task = rig.step()?.task;
    assert_eq!(rig.job_ids(), vec![format!("rung-main-{task}-a1")]);
    Ok(())
}

fn exceptions(rig: &Rig) -> String {
    std::fs::read_to_string(rig.ctx.home.join("exceptions.txt")).unwrap_or_default()
}

const MISSING_TSC: &str = "agent exit 127 in 1s\n\n--- stderr tail\nbash: tsc: command not found\n";

#[test]
fn an_environment_failure_holds_the_step_without_routing_or_retrying() -> R {
    let mut rig = rig(config(true))?;
    let id = first_job(&mut rig)?;
    rig.finish(&id, JobStatus::Finished, MISSING_TSC)?;
    rig.tick()?;
    let step = rig.step()?;
    assert_eq!(step.state, StepState::Waiting);
    assert_eq!(step.verdict.as_deref(), Some("operator-hold"));
    assert_eq!(rig.job_ids(), vec![id]);
    assert_eq!(rig.store.last_attempt(&step.task)?, 1);
    let rows = rig.store.steps_of(NEED, 1)?;
    assert!(rows.iter().all(|row| row.node != "rework1"), "{rows:?}");
    let text = exceptions(&rig);
    let env: Vec<&str> = text
        .lines()
        .filter(|line| line.contains("flow-env-failure"))
        .collect();
    assert_eq!(env.len(), 1, "{text}");
    assert!(env[0].contains("tool tsc"), "{text}");
    assert!(env[0].contains("expected the rung script to run"), "{text}");
    rig.tick()?;
    assert_eq!(exceptions(&rig).matches("flow-env-failure").count(), 1);
    Ok(())
}

#[test]
fn a_plain_exit_one_still_answers_fail() -> R {
    let mut rig = rig(config(true))?;
    let id = first_job(&mut rig)?;
    rig.finish(&id, JobStatus::Finished, "boom\nagent exit 1 in 8s")?;
    rig.tick()?;
    assert_eq!(rig.step()?.verdict.as_deref(), Some("fail"));
    assert!(!exceptions(&rig).contains("flow-env-failure"));
    Ok(())
}

#[test]
fn an_unconfigured_rung_error_names_expected_and_actual() -> R {
    let rig = rig(config(true))?;
    let step = rig.step()?;
    let err = job(&rig.ctx, "default", "nosuch", BASE, &step, 1).unwrap_err();
    let text = err.to_string();
    assert!(
        text.contains("expected a command in [pump.rungs] for target default")
            && text.contains("actual: not configured"),
        "{text}"
    );
    Ok(())
}
