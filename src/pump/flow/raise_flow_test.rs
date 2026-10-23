//! End to end through the shipped build flow's first verify pass: a failed
//! need rung goes to the judge, the raise check decides whether a higher rung
//! runs, and a failing pass sends the run to rework1 and then to build2.
//!
//! The rig is the real tick with the shipped registry, a `FakeMail`, and one
//! stub: the fanout handler answers `built` at once (the real one belongs to
//! another need). Every other job is started by its shipped handler and
//! finished here with a scripted reply, as a pool worker would.

use std::collections::BTreeMap;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::tick::tick_with;
use crate::error::VivariumError;
use crate::pump::config::{PumpConfig, RungsConfig};
use crate::pump::flow::advance::start::start;
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::{Registry, shipped};
use crate::pump::flow::index::{StepJobRow, StepRow, StepState};
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{self, JobOutcome, JobSpec, JobStatus, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};

type R = Result<(), VivariumError>;

const NEED: &str = "need-1";
const BASE: &str = "1111111111111111111111111111111111111111";
const TIP: &str = "2222222222222222222222222222222222222222";

const SLICED: &str = "+++\nanswer = \"sliced\"\n\n[[slice]]\nid = \"a\"\ntitle = \"first\"\n\
                      brief = \"do it\"\ntarget = \"default\"\nwrite_scope = [\"src/a.rs\"]\n+++\ncut";
const REWORK_SLICED: &str = "+++\nanswer = \"sliced\"\nmode = \"rework\"\n\n[[slice]]\n\
                             id = \"r\"\ntitle = \"fix\"\nbrief = \"fix it\"\ntarget = \"default\"\n\
                             write_scope = [\"src/a.rs\"]\n+++\ncut";
const AUDIT_RAISES_MAIN: &str =
    "+++\nanswer = \"residual\"\nraise_rung = \"main\"\n+++\nlooks fine, run main";
const JUDGE_RESIDUAL: &str =
    "+++\nanswer = \"residual\"\nreason = 'only minor leftovers'\n+++\nresidual";
const JUDGE_BLOCK: &str =
    "+++\nanswer = \"block\"\nreason = 'the rung failure is real'\n+++\nblock";

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

/// Stands in for the fanout handler: every fanout step is built at once.
struct BuiltAtOnce;

impl StepHandler for BuiltAtOnce {
    fn start(&self, _cx: &StartCx<'_>, _step: &StepRow) -> Result<StartAction, VivariumError> {
        Ok(StartAction::Decide {
            word: "built".into(),
        })
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

fn config() -> PumpConfig {
    let mut cfg = PumpConfig::default();
    cfg.rungs = RungsConfig {
        need: Some(String::from("cargo test")),
        main: Some(String::from("cargo test --all")),
        timeout: Some(900),
        pools: vec![String::from("build-pool")],
        ..RungsConfig::default()
    };
    cfg
}

fn script_host(host: &Arc<FakeHost>) {
    host.reply(
        "ls-remote origin refs/heads/vivi/need/",
        0,
        &format!("{TIP}\trefs/heads/vivi/need/{NEED}\n"),
    );
    host.reply(
        "need show",
        0,
        &serde_json::json!([
            {"handle": NEED, "message_id": "m1", "from": "operator@local",
             "subject": "Need", "body": "Build the thing"}
        ])
        .to_string(),
    );
}

/// A build run at entry `slice` over a live need branch, with `slice` waiting
/// for its slicer reply.
fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, host) = test_ctx(dir.path(), config());
    script_host(&host);
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("mind");
    MailWriter::open_record(&mut mail, NEED, "kind: build")?;
    start(&ctx, &store, &mut mail, NEED, Some("slice"))?;
    store.upsert_need_branch(&NeedBranchRow {
        need: NEED.into(),
        branch: format!("vivi/need/{NEED}"),
        base: Some(BASE.into()),
        status: "live".into(),
        run: 1,
        target: "default".into(),
        priors: Vec::new(),
    })?;
    let mut registry = shipped::<Jobs, FakeMail>(&ctx);
    registry.handler(FlowKind::Build, NodeClass::Fanout, Box::new(BuiltAtOnce));
    Ok(Rig {
        _dir: dir,
        ctx,
        store,
        mail,
        jobs: Jobs::default(),
        registry,
    })
}

fn receipt(exit: i32, message: &str) -> String {
    format!(
        "pool result j1: finished\npool p  slot s1  model m\nfrom main  default\n\
         push: \nagent exit {exit} in 12s, job 20s, started from 1234567890\n\n\
         --- receipt\n{message}\n\n--- stderr tail\n\n\n--- runner log tail\nlog line"
    )
}

impl Rig {
    /// Tick three times: a step opened by a code decision starts one tick later.
    fn settle(&mut self) -> R {
        for _ in 0..3 {
            tick_with(
                &self.ctx,
                &mut self.jobs,
                &self.store,
                &mut self.mail,
                &self.registry,
            )?;
        }
        Ok(())
    }

    fn step(&self, node: &str) -> Result<StepRow, VivariumError> {
        self.store
            .steps_of(NEED, 1)?
            .into_iter()
            .rev()
            .find(|row| row.node == node)
            .ok_or_else(|| {
                VivariumError::Message(format!(
                    "no step {node}; steps: {:?}",
                    self.store.steps_of(NEED, 1)
                ))
            })
    }

    fn has_step(&self, node: &str) -> Result<bool, VivariumError> {
        Ok(self
            .store
            .steps_of(NEED, 1)?
            .iter()
            .any(|row| row.node == node))
    }

    /// The newest job bound to `node`'s step task.
    fn job_of(&self, node: &str) -> Result<PoolJob, VivariumError> {
        let task = self.step(node)?.task;
        self.jobs
            .jobs
            .values()
            .filter(|job| job.task.as_deref() == Some(task.as_str()))
            .next_back()
            .cloned()
            .ok_or_else(|| VivariumError::Message(format!("no job for step {node}")))
    }

    fn finish(&mut self, node: &str, exit: i32, message: &str) -> R {
        let id = self.job_of(node)?.id;
        let job = self
            .jobs
            .jobs
            .get_mut(&id)
            .ok_or_else(|| VivariumError::Message(format!("no job {id}")))?;
        job.status = JobStatus::Finished;
        job.outcome = Some(JobOutcome {
            state: "finished".into(),
            detail: String::new(),
            receipt: receipt(exit, message),
            at: 0,
        });
        Ok(())
    }

    /// Answer the `slice` step, which no shipped handler starts: a finished
    /// job bound to its task, as a worker would leave behind.
    fn answer_slice(&mut self, message: &str) -> R {
        let step = self.step("slice")?;
        let id = "slice-j1";
        let mut spec = JobSpec::new(id, "slicer", "slicer", "s", "b");
        spec.task = Some(step.task.clone());
        jobs::enqueue(&mut self.jobs, spec)?;
        self.store.insert_step_job(
            NEED,
            &StepJobRow {
                job: id.into(),
                run: 1,
                node: "slice".into(),
                task: step.task.clone(),
                attempt: 1,
            },
        )?;
        self.store
            .set_step_state(&step.task, StepState::Running, None)?;
        self.finish("slice", 0, message)
    }

    /// Run to the point where audit1 and test1 are both enqueued.
    fn through_build1(&mut self) -> R {
        self.answer_slice(SLICED)?;
        self.settle()?;
        assert_eq!(self.step("build1")?.verdict.as_deref(), Some("built"));
        assert!(self.job_of("audit1").is_ok(), "audit1 has no job");
        assert!(self.job_of("test1").is_ok(), "test1 has no job");
        Ok(())
    }

    /// Audit1 asks for the main rung; the need rung fails; the judge job runs.
    fn through_failed_rung_to_judge(&mut self) -> R {
        self.through_build1()?;
        self.finish("audit1", 0, AUDIT_RAISES_MAIN)?;
        self.finish("test1", 1, "test result: FAILED")?;
        self.settle()
    }
}

#[test]
fn a_failed_rung_goes_to_the_judge_and_a_residual_ruling_raises_the_rung() -> R {
    let mut rig = rig()?;
    rig.through_failed_rung_to_judge()?;

    // j1 saw a residual audit and a failing rung: the judge runs.
    assert_eq!(rig.step("audit1")?.verdict.as_deref(), Some("residual"));
    assert_eq!(rig.step("test1")?.verdict.as_deref(), Some("fail"));
    assert_eq!(rig.step("judge1")?.state, StepState::Running);
    let judge = rig.job_of("judge1")?;
    assert_eq!(judge.role, "auditor");
    assert_eq!(judge.payload["class"], serde_json::json!("judge"));
    assert!(judge.body.contains("Judge mode"), "{}", judge.body);
    assert!(judge.body.contains("exit: 1"), "{}", judge.body);
    assert!(!rig.has_step("rework1")?);

    // A residual ruling goes through the raise check: audit1 named `main`.
    rig.finish("judge1", 0, JUDGE_RESIDUAL)?;
    rig.settle()?;
    assert_eq!(rig.step("judge1")?.verdict.as_deref(), Some("residual"));
    let raised = rig.job_of("raise1")?;
    assert_eq!(raised.kind, "rung");
    assert_eq!(raised.command.as_deref(), Some("cargo test --all"));
    assert_eq!(raised.payload["rung"], serde_json::json!("main"));
    assert!(!rig.has_step("rework1")?);

    // The raised rung fails: rework1 runs the slicer in rework mode.
    rig.finish("raise1", 1, "test result: FAILED")?;
    rig.settle()?;
    assert_eq!(rig.step("raise1")?.verdict.as_deref(), Some("fail"));
    assert_eq!(rig.step("rework1")?.state, StepState::Running);
    let rework = rig.job_of("rework1")?;
    assert_eq!(rework.role, "slicer");
    assert!(rework.body.starts_with("mode: rework"), "{}", rework.body);
    assert!(!rig.has_step("build2")?);

    // A rework reply `sliced` opens build2 (stubbed to built at once).
    rig.finish("rework1", 0, REWORK_SLICED)?;
    rig.settle()?;
    assert_eq!(rig.step("rework1")?.verdict.as_deref(), Some("sliced"));
    assert_eq!(rig.step("build2")?.verdict.as_deref(), Some("built"));
    assert!(rig.has_step("audit2")?);
    assert!(rig.has_step("test2")?);
    Ok(())
}

#[test]
fn a_judge_block_goes_straight_to_rework() -> R {
    let mut rig = rig()?;
    rig.through_failed_rung_to_judge()?;
    assert_eq!(rig.step("judge1")?.state, StepState::Running);

    rig.finish("judge1", 0, JUDGE_BLOCK)?;
    rig.settle()?;

    assert_eq!(rig.step("judge1")?.verdict.as_deref(), Some("block"));
    assert!(!rig.has_step("raise1")?, "a block skips the raise check");
    assert_eq!(rig.step("rework1")?.state, StepState::Running);
    let rework = rig.job_of("rework1")?;
    assert_eq!(rework.role, "slicer");
    assert!(rework.body.starts_with("mode: rework"), "{}", rework.body);
    assert!(
        rework.body.contains("the rung failure is real"),
        "the judge reason reaches the rework brief: {}",
        rework.body
    );
    Ok(())
}
