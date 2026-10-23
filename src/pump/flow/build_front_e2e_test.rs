//! End to end: the build flow's front half on a real mailspace, driven by the
//! real tick and the shipped registry.
//!
//! Intake starts run 1 of a need with an open pump record. The planner,
//! admitter and slicer steps become pool jobs; the test stands in for the pool
//! worker by finishing each job with an envelope receipt and by posting the
//! same `pool result` reply on the step task, where the next handler reads it.
//! The slicer's reply fans out into two slices (one depends on the other): the
//! ready-slices phase queues only the free slice, on the recorded base. Once
//! the test closes both slice tasks `merged`, the fanout settles and build1
//! answers `built`, so routing opens audit1 and test1.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::pump::config::RungsConfig;

use crate::error::VivariumError;
use crate::pump::fixture::{Project, project};
use crate::pump::flow::advance::start::SENTINEL_NODE;
use crate::pump::flow::fanout::{self, VERDICT_MERGED};
use crate::pump::flow::handlers;
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::flow::mail::{MailReader, MailspacePort};
use crate::pump::flow::need_branch::{self, LIVE};
use crate::pump::flow::step_inputs::POOL_RESULT_MARKER;
use crate::pump::flow::tick::tick_with;
use crate::pump::host::{Ctx, FakeHost};
use crate::pump::jobs::{JobOutcome, JobStatus, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

/// The pump's mailspace identity: the default `[pump] reply_from`.
const PUMP: &str = "mind";
/// The main tip every ls-remote of the main ref answers.
const BASE: &str = "3333333333333333333333333333333333333333";
/// Ticks a stage may take before the test gives up.
const MAX_TICKS: usize = 8;

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
    _project: Project,
    ctx: Ctx,
    host: std::sync::Arc<FakeHost>,
    store: PumpStore,
    port: MailspacePort,
    jobs: Jobs,
    need: String,
}

/// A role active on the test pool.
fn role(name: &str, kind: &str, read_only: Option<bool>) -> RoutedRole {
    RoutedRole {
        name: name.into(),
        kind: Some(kind.into()),
        status: "active".into(),
        pool: Some("test-planner-pool".into()),
        read_only,
        model: None,
        models: Vec::new(),
    }
}

fn rig() -> Result<Rig, VivariumError> {
    let p = project();
    let need = p.need("build something in two slices");
    let host = p.host.clone();
    host.reply(
        "ls-remote origin main",
        0,
        &format!("{BASE}\trefs/heads/main\n"),
    );
    host.reply("ls-remote origin refs/heads/vivi/need/", 0, "");
    // The audit brief reads the full need thread as JSON.
    let thread = serde_json::json!([
        {"handle": need, "message_id": "m1", "from": "operator@local",
         "subject": "Need", "body": crate::letter::fixture::need_body(&["it works"], "build something in two slices")}
    ]);
    host.reply("need show", 0, &thread.to_string());
    let mut ctx = p.pump_ctx();
    let mut routing = (*ctx.routing).clone();
    routing.roles.push(role("admitter", "admitter", Some(true)));
    routing.roles.push(role("slicer", "slicer", Some(true)));
    routing.roles.push(role("hand", "hand", None));
    ctx = ctx.with_routing(routing);
    // test1 is a need-rung step: it needs a configured command and a pool.
    Arc::make_mut(&mut ctx.cfg).rungs = RungsConfig {
        need: Some("cargo test".into()),
        timeout: Some(900),
        pools: vec!["test-planner-pool".into()],
        ..RungsConfig::default()
    };
    assert_ne!(ctx.cfg.from, ctx.cfg.reply_from);
    assert_eq!(ctx.cfg.reply_from, PUMP);
    let store = PumpStore::open(&ctx.home.join(DB_FILENAME))?;
    let mut port = MailspacePort {
        mailspace: p.mailspace.clone(),
        pump_identity: PUMP.into(),
    };
    crate::pump::flow::mail::MailWriter::open_record(
        &mut port,
        &need,
        &crate::letter::fixture::record_body(&need, "build"),
    )?;
    Ok(Rig {
        _project: p,
        ctx,
        host,
        store,
        port,
        jobs: Jobs::default(),
        need,
    })
}

fn receipt(job: &str, message: &str) -> String {
    format!(
        "{POOL_RESULT_MARKER}{job}: finished\npool p  slot s1  model m\nfrom main  default\n\
         push: \nagent exit 0 in 12s, job 20s, started from 1234567890\n\n\
         --- receipt\n{message}\n\n--- stderr tail\n\n\n--- runner log tail\nlog line"
    )
}

fn slicer_reply() -> &'static str {
    "+++\nanswer = \"sliced\"\n\n\
     [[slice]]\nid = \"a\"\ntitle = \"first\"\nbrief = 'build a'\ntarget = \"default\"\n\
     write_scope = [\"src/a.rs\"]\ndeps = []\n\n\
     [[slice]]\nid = \"b\"\ntitle = \"second\"\nbrief = 'build b on a'\ntarget = \"default\"\n\
     write_scope = [\"src/b.rs\"]\ndeps = [\"a\"]\n+++\nsliced"
}

impl Rig {
    fn tick(&mut self) -> R {
        let registry = handlers::shipped::<Jobs, MailspacePort>(&self.ctx);
        tick_with(
            &self.ctx,
            &mut self.jobs,
            &self.store,
            &mut self.port,
            &registry,
        )
    }

    fn steps(&self) -> Result<Vec<StepRow>, VivariumError> {
        self.store.steps_of(&self.need, 1)
    }

    fn step(&self, node: &str) -> Result<Option<StepRow>, VivariumError> {
        Ok(self.steps()?.into_iter().find(|row| row.node == node))
    }

    /// The in-flight job of step `node`, if any.
    fn open_job(&self, node: &str) -> Result<Option<String>, VivariumError> {
        for job in self.jobs.jobs.values() {
            if job.status.is_terminal() {
                continue;
            }
            if self
                .store
                .step_job(&job.id)?
                .is_some_and(|row| row.node == node)
            {
                return Ok(Some(job.id.clone()));
            }
        }
        Ok(None)
    }

    /// Tick until `node` has an in-flight job, then finish it with `message`
    /// and post the same result on the step task.
    fn work(&mut self, node: &str, message: &str) -> R {
        for _ in 0..MAX_TICKS {
            self.tick()?;
            if let Some(id) = self.open_job(node)? {
                return self.finish(&id, message);
            }
        }
        Err(VivariumError::Message(format!(
            "no job for {node}: steps {:?}, exceptions {}, log {:?}",
            self.steps()?,
            self.exceptions(),
            self.host.logs()
        )))
    }

    fn finish(&mut self, id: &str, message: &str) -> R {
        let row = self
            .store
            .step_job(id)?
            .ok_or_else(|| VivariumError::Message(format!("job {id} has no step row")))?;
        let text = receipt(id, message);
        let posted = text.clone();
        let job = self
            .jobs
            .jobs
            .get_mut(id)
            .ok_or_else(|| VivariumError::Message(format!("job {id} vanished")))?;
        job.status = JobStatus::Finished;
        job.outcome = Some(JobOutcome {
            state: "finished".into(),
            detail: String::new(),
            receipt: text,
            at: 0,
        });
        // The pool records a settled job as a note reply on the task.
        self.port.mailspace.record_pool_note(
            &row.task,
            "pool_collected",
            &format!("{{\"job\":\"{id}\"}}"),
            &posted,
        )?;
        Ok(())
    }

    fn exceptions(&self) -> String {
        std::fs::read_to_string(self.ctx.home.join("exceptions.txt")).unwrap_or_default()
    }

    /// The slice step row `slice:<id>`.
    fn slice(&self, id: &str) -> Result<StepRow, VivariumError> {
        self.step(&format!("slice:{id}"))?.ok_or_else(|| {
            VivariumError::Message(format!(
                "no slice row {id}: {:?}, exceptions {}",
                self.steps(),
                self.exceptions()
            ))
        })
    }
}

#[test]
fn intake_to_a_settled_fanout_opens_audit_and_test() -> R {
    let mut rig = rig()?;
    assert!(rig.store.runs_of(&rig.need)?.is_empty());

    // Intake starts run 1 at the flow's first entry.
    rig.tick()?;
    let runs = rig.store.runs_of(&rig.need)?;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].kind, "build");
    assert_eq!(runs[0].entry, "plan");

    // plan -> admit1 -> slice, each a pool job finished by the test.
    rig.work(
        "plan",
        "+++\nanswer = \"planned\"\n+++\nthe plan: two slices",
    )?;
    rig.work("admit1", "+++\nanswer = \"admitted\"\n+++\nadmitted")?;
    rig.work("slice", slicer_reply())?;

    // build1 expands the fanout: two slice tasks bound to the need, and only
    // the slice with no dependency is dispatched, from the recorded base.
    for _ in 0..MAX_TICKS {
        rig.tick()?;
        if rig.step("slice:a")?.is_some() && rig.open_job("slice:a")?.is_some() {
            break;
        }
    }
    let a = rig.slice("a")?;
    let b = rig.slice("b")?;
    let bound = rig.port.bound_units(&rig.need)?;
    assert!(bound.contains(&a.task), "{bound:?}");
    assert!(bound.contains(&b.task), "{bound:?}");
    assert_eq!(rig.step("build1")?.unwrap().state, StepState::Running);

    let job_a = format!("slice-{}-a1", a.task);
    let queued: Vec<&String> = rig
        .jobs
        .jobs
        .keys()
        .filter(|id| id.starts_with("slice-"))
        .collect();
    assert_eq!(queued, [&job_a], "only the slice with no deps is queued");
    assert_eq!(rig.jobs.jobs[&job_a].from_ref.as_deref(), Some(BASE));
    assert_eq!(rig.slice("a")?.state, StepState::Running);
    assert_eq!(rig.slice("b")?.state, StepState::Pending);
    let branch = rig.store.need_branch(&rig.need)?.unwrap();
    assert_eq!(branch.status, LIVE);
    assert_eq!(branch.base.as_deref(), Some(BASE));
    assert_eq!(branch.branch, need_branch::branch(&rig.need));

    // Both slices close merged; the fanout settles and build1 answers built.
    fanout::close_slice(&rig.store, &mut rig.port, &a.task, VERDICT_MERGED)?;
    fanout::close_slice(&rig.store, &mut rig.port, &b.task, VERDICT_MERGED)?;
    for _ in 0..MAX_TICKS {
        rig.tick()?;
        if rig.step("audit1")?.is_some() {
            break;
        }
    }
    let build1 = rig.step("build1")?.unwrap();
    assert_eq!(build1.state, StepState::Done);
    assert_eq!(build1.verdict.as_deref(), Some("built"));

    // Routing opened audit1 and test1. N5's audit handler starts the audit
    // job; test1 is a rung step and runs its configured command.
    let audit1 = rig.step("audit1")?.expect("audit1 opened");
    let test1 = rig.step("test1")?.expect("test1 opened");
    assert_eq!(audit1.state, StepState::Running);
    assert!(rig.open_job("audit1")?.is_some());
    assert_eq!(test1.state, StepState::Running);
    assert!(rig.open_job("test1")?.is_some());

    // The run is still live, the sentinel is untouched, and no alarm fired.
    assert_eq!(rig.store.live_runs()?.len(), 1);
    assert_eq!(rig.step(SENTINEL_NODE)?.unwrap().state, StepState::Running);
    assert_eq!(rig.exceptions(), "");
    Ok(())
}
