//! Repair rung-main and merge-main steps: jobs at the attempt commit, a merge
//! cut from the attempt's base, and the rung verdict reply.

use std::collections::BTreeMap;

use tempfile::{TempDir, tempdir};

use super::{RepairMerge, RepairRung, register};
use crate::error::VivariumError;
use crate::mailspace::marked_line;
use crate::pump::config::{PumpConfig, RungsConfig};
use crate::pump::flow::advance::start::start;
use crate::pump::flow::def::FlowKind;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{RunRow, StepJobRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::repair::{self, AttemptFacts};
use crate::pump::flow::tick::tick_with;
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::jobs::{self, JobOutcome, JobSpec, JobStatus, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

const NEED: &str = "n1";
const BASE: &str = "1111111111111111111111111111111111111111";
const COMMIT: &str = "2222222222222222222222222222222222222222";

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

fn rig(facts: bool) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let mut cfg = PumpConfig::default();
    cfg.rungs = RungsConfig {
        main: Some(String::from("cargo test")),
        timeout: Some(900),
        pools: vec![String::from("build-pool")],
        ..RungsConfig::default()
    };
    let (mut ctx, _host) = test_ctx(dir.path(), cfg);
    let mut routing = (*ctx.routing).clone();
    routing.roles.push(RoutedRole {
        name: String::from("merge"),
        kind: Some(String::from("merge")),
        status: String::from("active"),
        pool: Some(String::from("test-planner-pool")),
        read_only: None,
        model: None,
        models: Vec::new(),
    });
    ctx = ctx.with_routing(routing);
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("mind");
    MailWriter::open_record(&mut mail, NEED, "kind: repair\ntarget: default")?;
    start(&ctx, &store, &mut mail, NEED, Some("fix1"))?;
    let mut rig = Rig {
        _dir: dir,
        ctx,
        store,
        mail,
        jobs: Jobs::default(),
        registry: Registry::new(),
    };
    register(&mut rig.registry);
    if facts {
        rig.post_facts()?;
    }
    Ok(rig)
}

impl Rig {
    fn fix1(&self) -> Result<StepRow, VivariumError> {
        self.step("fix1")
    }

    fn step(&self, node: &str) -> Result<StepRow, VivariumError> {
        self.store
            .steps_of(NEED, 1)?
            .into_iter()
            .rev()
            .find(|row| row.node == node)
            .ok_or_else(|| VivariumError::Message(format!("no step {node}")))
    }

    /// The fix1 task carries the attempt facts reply.
    fn post_facts(&mut self) -> R {
        let facts = AttemptFacts {
            branch: repair::branch(NEED, 1, 1),
            commit: COMMIT.into(),
            base: BASE.into(),
        };
        let task = self.fix1()?.task;
        self.mail.reply(
            &task,
            &marked_line(repair::ATTEMPT_MARKER, &repair::facts_text(&facts)),
        )
    }

    /// A step row of `node` with its own task.
    fn add_step(&mut self, node: &str, state: StepState) -> Result<StepRow, VivariumError> {
        let task = self.mail.send_step(
            "mind",
            "mind",
            node,
            &format!("pump-step: {NEED} r1 {node}"),
            &[],
        )?;
        let row = StepRow {
            run: 1,
            node: node.into(),
            task,
            state,
            verdict: None,
        };
        self.store.insert_step(NEED, &row)?;
        Ok(row)
    }

    fn action(
        &self,
        handler: &dyn StepHandler,
        step: &StepRow,
    ) -> Result<StartAction, VivariumError> {
        let def = load(FlowKind::Repair)?;
        let run = RunRow {
            need: NEED.into(),
            run: 1,
            flow: "repair".into(),
            flow_version: 1,
            kind: "repair".into(),
            entry: "fix1".into(),
            state: "live".into(),
        };
        let cx = StartCx {
            ctx: &self.ctx,
            store: &self.store,
            mail: &self.mail,
            run: &run,
            def: &def,
        };
        handler.start(&cx, step)
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
}

fn job(action: StartAction) -> Box<JobSpec> {
    match action {
        StartAction::Job(spec) => spec,
        other => panic!("expected a job, got {other:?}"),
    }
}

#[test]
fn rung1_starts_a_main_rung_job_at_the_attempt_commit() -> R {
    let mut rig = rig(true)?;
    let rung = rig.add_step("rung1", StepState::Pending)?;
    let spec = job(rig.action(&RepairRung, &rung)?);
    assert_eq!(spec.id, format!("rung-main-{}-a1", rung.task));
    assert_eq!(spec.kind, "rung");
    assert_eq!(spec.from_ref.as_deref(), Some(COMMIT));
    assert_eq!(spec.target.as_deref(), Some("default"));
    assert_eq!(spec.task.as_deref(), Some(rung.task.as_str()));
    assert_eq!(spec.command.as_deref(), Some("cargo test"));
    Ok(())
}

#[test]
fn merge1_starts_a_merge_cut_from_the_attempt_base() -> R {
    let mut rig = rig(true)?;
    let merge = rig.add_step("merge1", StepState::Pending)?;
    let spec = job(rig.action(&RepairMerge, &merge)?);
    assert_eq!(spec.kind, "merge");
    assert_eq!(spec.from_ref.as_deref(), Some(BASE));
    assert_eq!(spec.publish_contains.as_deref(), Some(COMMIT));
    assert_eq!(spec.publish_ref.as_deref(), Some("refs/heads/main"));
    assert!(spec.id.starts_with(&format!("merge-main-{}-", merge.task)));
    Ok(())
}

#[test]
fn no_published_attempt_is_an_error() -> R {
    let mut rig = rig(false)?;
    let rung = rig.add_step("rung1", StepState::Pending)?;
    let merge = rig.add_step("merge1", StepState::Pending)?;
    for (handler, step) in [
        (&RepairRung as &dyn StepHandler, &rung),
        (&RepairMerge, &merge),
    ] {
        let err = rig.action(handler, step).unwrap_err();
        assert!(
            err.to_string().contains("no published repair attempt"),
            "{err}"
        );
    }
    Ok(())
}

#[test]
fn the_rung_collector_posts_the_verdict_reply_before_advancing() -> R {
    let mut rig = rig(true)?;
    let fix = rig.fix1()?;
    rig.store
        .set_step_state(&fix.task, StepState::Running, None)?;
    let rung = rig.add_step("rung1", StepState::Running)?;
    let spec = job(rig.action(&RepairRung, &rung)?);
    let id = spec.id.clone();
    jobs::enqueue(&mut rig.jobs, *spec)?;
    rig.store.insert_step_job(
        NEED,
        &StepJobRow {
            job: id.clone(),
            run: 1,
            node: "rung1".into(),
            task: rung.task.clone(),
            attempt: 1,
        },
    )?;
    let queued = rig.jobs.jobs.get_mut(&id).unwrap();
    queued.status = JobStatus::Finished;
    queued.outcome = Some(JobOutcome {
        state: "finished".into(),
        detail: String::new(),
        receipt: "boom\nagent exit 1 in 8s".into(),
        at: 1,
    });
    rig.tick()?;
    let done = rig.step("rung1")?;
    assert_eq!(done.state, StepState::Done);
    assert_eq!(done.verdict.as_deref(), Some("fail"));
    let replies = &rig.mail.tasks[&rung.task].replies;
    let verdict = replies
        .iter()
        .find(|reply| reply.starts_with(repair::RUNG_MARKER))
        .unwrap_or_else(|| panic!("no rung verdict reply: {replies:?}"));
    assert!(verdict.contains("exit 1"), "{verdict}");
    Ok(())
}
