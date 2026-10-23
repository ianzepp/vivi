//! End to end through the tick: after the fanout, the audit and the need rung
//! run side by side and join to `verified` (merge opens) or `raised` (a higher
//! rung runs).
//!
//! The fanout and slicer handlers are stubs that answer at once (their real
//! handlers belong to other needs); every other handler is the shipped one.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::json;
use tempfile::{TempDir, tempdir};

use crate::error::VivariumError;
use crate::pump::config::{PumpConfig, RungsConfig};
use crate::pump::flow::advance::start::start;
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::{self, Registry};
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::tick::tick_with;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobOutcome, JobStatus, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};

type R = Result<(), VivariumError>;
type Reg = Registry<Jobs, FakeMail>;

const NEED: &str = "n1";
const BASE: &str = "1111111111111111111111111111111111111111";
const TIP: &str = "2222222222222222222222222222222222222222";

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

/// A handler that answers its step with `word` at once.
struct Answer(&'static str);

impl StepHandler for Answer {
    fn start(&self, _cx: &StartCx<'_>, _step: &StepRow) -> Result<StartAction, VivariumError> {
        Ok(StartAction::Decide {
            word: self.0.into(),
        })
    }
}

struct Rig {
    _dir: TempDir,
    ctx: Ctx,
    store: PumpStore,
    mail: FakeMail,
    jobs: Jobs,
    registry: Reg,
}

/// A build run started at `slice`, with a live need branch, a remote tip and
/// the rungs configured.
fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let mut cfg = PumpConfig::default();
    cfg.rungs = RungsConfig {
        need: Some(String::from("cargo test")),
        main: Some(String::from("cargo test --all")),
        timeout: Some(900),
        pools: vec![String::from("build-pool")],
        ..RungsConfig::default()
    };
    let (ctx, host) = test_ctx(dir.path(), cfg);
    replies(&host)?;
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("mind");
    MailWriter::open_record(&mut mail, NEED, "kind: build")?;
    store.upsert_need_branch(&NeedBranchRow {
        need: NEED.into(),
        branch: format!("vivi/need/{NEED}"),
        base: Some(BASE.into()),
        status: "live".into(),
        run: 1,
        target: "default".into(),
        priors: Vec::new(),
    })?;
    start(&ctx, &store, &mut mail, NEED, Some("slice"))?;
    let mut registry: Reg = handlers::shipped(&ctx);
    registry.node_handler(FlowKind::Build, "slice", Box::new(Answer("sliced")));
    registry.handler(
        FlowKind::Build,
        NodeClass::Fanout,
        Box::new(Answer("built")),
    );
    Ok(Rig {
        _dir: dir,
        ctx,
        store,
        mail,
        jobs: Jobs::default(),
        registry,
    })
}

/// The remote tip of the need branch and the need thread.
fn replies(host: &Arc<FakeHost>) -> R {
    host.reply(
        "ls-remote origin refs/heads/vivi/need/",
        0,
        &format!("{TIP}\trefs/heads/vivi/need/{NEED}\n"),
    );
    let thread = serde_json::to_string(&json!([
        {"handle": NEED, "message_id": "m1", "from": "operator@local",
         "subject": "Need", "body": "Build the thing"}
    ]))
    .map_err(|e| VivariumError::Other(e.to_string()))?;
    host.reply("need show", 0, &thread);
    Ok(())
}

fn receipt(message: &str) -> String {
    format!(
        "pool result j1: finished\npool p  slot s1  model m\nfrom main  default\n\
         push: \nagent exit 0 in 12s, job 20s, started from 1234567890\n\n\
         --- receipt\n{message}\n\n--- stderr tail\n\n\n--- runner log tail\nlog line"
    )
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

    fn step(&self, node: &str) -> Result<StepRow, VivariumError> {
        self.store
            .steps_of(NEED, 1)?
            .into_iter()
            .rev()
            .find(|row| row.node == node)
            .ok_or_else(|| VivariumError::Message(format!("no step {node}")))
    }

    /// The id of the job of `node`'s step.
    fn job_of(&self, node: &str) -> Result<String, VivariumError> {
        self.jobs
            .jobs
            .values()
            .find(|job| job.payload["node"] == json!(node))
            .map(|job| job.id.clone())
            .ok_or_else(|| VivariumError::Message(format!("no job for {node}")))
    }

    fn finish(&mut self, node: &str, receipt: &str) -> R {
        let id = self.job_of(node)?;
        let job = self
            .jobs
            .jobs
            .get_mut(&id)
            .ok_or_else(|| VivariumError::Message(format!("no job {id}")))?;
        job.status = JobStatus::Finished;
        job.outcome = Some(JobOutcome {
            state: "finished".into(),
            detail: String::new(),
            receipt: receipt.to_owned(),
            at: 0,
        });
        Ok(())
    }

    /// Tick through the slice and build1 steps until the audit and the need
    /// rung are queued together.
    fn to_verify(&mut self) -> R {
        for _ in 0..3 {
            self.tick()?;
        }
        Ok(())
    }

    /// Answer the audit with `envelope` and the need rung with exit 0, then tick.
    fn verify_with(&mut self, envelope: &str) -> R {
        self.finish("audit1", &receipt(envelope))?;
        self.finish("test1", &receipt("tests ok"))?;
        self.tick()
    }
}

#[test]
fn after_the_fanout_the_audit_and_the_need_rung_are_queued_together() -> R {
    let mut rig = rig()?;
    rig.to_verify()?;

    assert_eq!(rig.step("slice")?.verdict.as_deref(), Some("sliced"));
    assert_eq!(rig.step("build1")?.verdict.as_deref(), Some("built"));
    assert_eq!(rig.jobs.jobs.len(), 2, "{:?}", rig.jobs.jobs.keys());
    let audit = &rig.jobs.jobs[&rig.job_of("audit1")?];
    assert_eq!(audit.role, "auditor");
    assert!(audit.read_only);
    let rung = &rig.jobs.jobs[&rig.job_of("test1")?];
    assert_eq!(rung.kind, "rung");
    assert_eq!(rung.command.as_deref(), Some("cargo test"));
    assert_eq!(rig.step("audit1")?.state, StepState::Running);
    assert_eq!(rig.step("test1")?.state, StepState::Running);
    Ok(())
}

#[test]
fn a_clean_audit_and_a_passing_rung_join_to_verified_and_open_the_merge() -> R {
    let mut rig = rig()?;
    rig.to_verify()?;

    rig.verify_with("+++\nanswer = \"clean_pass\"\n+++\nthe range is clean")?;

    assert_eq!(rig.step("audit1")?.verdict.as_deref(), Some("clean_pass"));
    assert_eq!(rig.step("test1")?.verdict.as_deref(), Some("pass"));
    for other in ["judge1", "raise1", "rework1"] {
        assert!(rig.step(other).is_err(), "join j1 opened {other}");
    }
    assert_eq!(rig.step("merge")?.state, StepState::Pending);
    let payload = rig
        .store
        .step_payload(&rig.step("audit1")?.task)?
        .ok_or_else(|| VivariumError::Message("no audit payload".into()))?;
    assert_eq!(payload.class, "auditor");
    assert_eq!(payload.payload["front"]["answer"], json!("clean_pass"));
    Ok(())
}

#[test]
fn a_residual_audit_that_raises_a_rung_joins_to_raised_and_queues_that_rung() -> R {
    let mut rig = rig()?;
    rig.to_verify()?;

    rig.verify_with("+++\nanswer = \"residual\"\nraise_rung = \"main\"\n+++\nrun the main rung")?;

    for other in ["merge", "judge1", "rework1"] {
        assert!(rig.step(other).is_err(), "join j1 opened {other}");
    }
    let task = rig.step("raise1")?.task;
    let rung = &rig.jobs.jobs[&format!("rung-main-{task}-a1")];
    assert_eq!(rung.command.as_deref(), Some("cargo test --all"));
    assert_eq!(rig.step("raise1")?.state, StepState::Running);
    Ok(())
}

#[test]
fn a_block_with_a_lost_rung_routes_to_rework_and_shows_no_exit() -> R {
    let mut rig = rig()?;
    rig.to_verify()?;

    let a1 = rig.job_of("test1")?;
    let task = rig.step("test1")?.task;
    let a2 = format!("rung-need-{task}-a2");
    for id in [a1, a2.clone()] {
        let job = rig
            .jobs
            .jobs
            .get_mut(&id)
            .ok_or_else(|| VivariumError::Message(format!("no job {id}")))?;
        job.status = JobStatus::Lost;
        if id != a2 {
            rig.tick()?;
        }
    }
    rig.finish(
        "audit1",
        &receipt("+++\nanswer = \"block_range\"\n+++\nstill broken"),
    )?;
    rig.tick()?;
    rig.tick()?;

    assert_eq!(rig.step("test1")?.verdict.as_deref(), Some("failed"));
    assert_eq!(rig.step("audit1")?.verdict.as_deref(), Some("block_range"));
    assert!(
        rig.step("cto1").is_err(),
        "a lost rung sent the block to the CTO"
    );
    let id = rig.job_of("rework1")?;
    let body = &rig.jobs.jobs[&id].body;
    assert!(body.contains("exit: none"), "{body}");
    Ok(())
}
