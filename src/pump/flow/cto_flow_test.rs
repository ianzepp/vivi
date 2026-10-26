//! End to end through the shipped build flow's second verify pass: a pass that
//! fails again reaches the CTO, and each CTO word restarts the need at its
//! entry or locks it.
//!
//! The run starts at `rework1`, so pass 2 is the whole story. The rework
//! slicer answers `sliced`, a stub handler answers `built` for the fanout
//! node `build2` (the real fanout belongs to another need), the audit answers
//! `block_range` and the need rung passes. Every job is finished by hand with
//! a scripted receipt, as a pool worker would.

use std::collections::BTreeMap;
use tempfile::{TempDir, tempdir};

use crate::error::VivariumError;
use crate::letter::fixture::record_body;
use crate::letter::fixture::step_body;
use crate::pump::config::{PumpConfig, RungsConfig};
use crate::pump::flow::advance::start::start;
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::fake_mail::{FakeMail, FakeTask};
use crate::pump::flow::fanout::slice_node;
use crate::pump::flow::handlers::{Registry, shipped};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::mail::{MailReader, MailWriter, TaskState, TaskStatus};
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::tick::tick_with;
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::jobs::{JobOutcome, JobStatus, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

const NEED: &str = "n1";
const BASE: &str = "0123456789abcdef0123456789abcdef01234567";
/// Ticks a stage may take before the test gives up.
const MAX_TICKS: usize = 8;

const SLICED: &str = "+++\nanswer = \"sliced\"\nmode = \"rework\"\n\n[[slice]]\nid = \"fix-1\"\n\
                      title = 'Fix it'\nbrief = 'fix the finding'\ntarget = \"default\"\n\
                      write_scope = [\"src/a.rs\"]\n+++\ncut one rework slice";
const BLOCK_RANGE: &str = "+++\nanswer = \"block_range\"\n+++\nthe range is still broken";
const BLOCK_WITH_FINDING: &str = "+++\nanswer = \"block_range\"\n\n[[finding]]\nid = \"F-1\"\n\
                                  content = 'the range is still broken'\npaths = [\"src/a.rs\"]\n\
                                  recheck_trigger = 'a.rs changes'\n+++\nthe range is still broken";
const FAILING_TEST: &str = "pump::x::y";

/// A planned envelope whose body is the stored plan the admission reads.
const PLANNED_ONE: &str = "+++\nanswer = \"planned\"\n+++\nPLAN-ONE BODY";
/// The revised planned envelope.
const PLANNED_TWO: &str = "+++\nanswer = \"planned\"\n+++\nPLAN-TWO BODY";
/// An admitter admitting the plan it reads.
const ADMITTED: &str = "+++\nanswer = \"admitted\"\n+++\nadmitted";
/// An admitter asking for a revision.
const REVISE: &str =
    "+++\nanswer = \"revise\"\nrequired_changes = ['tighten the scope']\n+++\nrevise";
/// The slicer handing the decision to the CTO.
const ESCALATE: &str = "+++\nanswer = \"escalate\"\nreason = 'needs a ruling'\n+++\nescalate";
/// The plain slicer's `sliced` envelope: two declared slices, the second on
/// the first.
const SLICED_PLAN: &str = "+++\nanswer = \"sliced\"\nmode = \"slice\"\n\n\
     [[slice]]\nid = \"a\"\ntitle = \"first\"\nbrief = 'build a'\ntarget = \"default\"\n\
     write_scope = [\"src/a.rs\"]\ndeps = []\n\n\
     [[slice]]\nid = \"b\"\ntitle = \"second\"\nbrief = 'build b on a'\ntarget = \"default\"\n\
     write_scope = [\"src/b.rs\"]\ndeps = [\"a\"]\n+++\nsliced";

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

/// Stands in for the fanout handler: the rework slices are built at once.
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
    /// Whether the tick uses the shipped fanout instead of the stub.
    real_fanout: bool,
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

/// A build need with a live need branch and a run started at `entry`.
fn rig(entry: &str) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, host) = test_ctx(dir.path(), config());
    host.reply(
        "ls-remote",
        0,
        "0123456789abcdef0123456789abcdef01234567\trefs/heads/main\n",
    );
    host.reply(
        "need show",
        0,
        &serde_json::json!([
            {"handle": NEED, "message_id": "m1", "from": "operator@local",
             "subject": "Need", "body": crate::letter::fixture::need_body(&["it works"], "Build the thing")}
        ])
        .to_string(),
    );
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("mind");
    MailWriter::open_record(&mut mail, NEED, &record_body(NEED, "build"))?;
    store.upsert_need_branch(&NeedBranchRow {
        need: NEED.into(),
        branch: format!("vivi/need/{NEED}"),
        base: Some(BASE.into()),
        status: "live".into(),
        run: 1,
        target: "default".into(),
        priors: Vec::new(),
    })?;
    start(&ctx, &store, &mut mail, NEED, Some(entry))?;
    Ok(Rig {
        _dir: dir,
        ctx,
        store,
        mail,
        jobs: Jobs::default(),
        real_fanout: false,
    })
}

/// A build need at `plan` with the front roles routed, the real fanout armed,
/// and the need letter readable as the root task the planner brief reads.
fn plan_rig() -> Result<Rig, VivariumError> {
    let mut rig = rig("plan")?;
    let mut routing = (*rig.ctx.routing).clone();
    for (name, read_only) in [("admitter", Some(true)), ("slicer", Some(true))] {
        routing.roles.push(RoutedRole {
            name: name.into(),
            kind: Some(name.into()),
            status: String::from("active"),
            pool: Some(String::from("test-planner-pool")),
            read_only,
            model: None,
            models: Vec::new(),
        });
    }
    rig.ctx = rig.ctx.with_routing(routing);
    rig.real_fanout = true;
    rig.mail.tasks.insert(
        NEED.into(),
        FakeTask {
            from: String::from("operator@local"),
            to: String::from("mind"),
            subject: NEED.into(),
            body: crate::letter::fixture::need_body(&["it works"], "Build the thing"),
            depends_on: Vec::new(),
            replies: Vec::new(),
            state: TaskState {
                status: TaskStatus::Open,
                closer: None,
                verdict: None,
            },
            branch: None,
            commit: None,
        },
    );
    Ok(rig)
}

fn receipt(exit: i32, message: &str) -> String {
    format!(
        "pool result j1: finished\npool p  slot s1  model m\nfrom main  default\n\
         push: \nagent exit {exit} in 12s, job 20s, started from 1234567890\n\n\
         --- receipt\n{message}\n\n--- stderr tail\n\n\n--- runner log tail\nlog line"
    )
}

impl Rig {
    fn registry(&self) -> Registry<Jobs, FakeMail> {
        let mut registry = shipped(&self.ctx);
        if !self.real_fanout {
            registry.handler(FlowKind::Build, NodeClass::Fanout, Box::new(BuiltAtOnce));
        }
        registry
    }

    fn tick(&mut self) -> R {
        let registry = self.registry();
        tick_with(
            &self.ctx,
            &mut self.jobs,
            &self.store,
            &mut self.mail,
            &registry,
        )
    }

    /// Tick until `done` holds.
    fn tick_until(&mut self, what: &str, done: impl Fn(&Rig) -> bool) -> R {
        for _ in 0..MAX_TICKS {
            self.tick()?;
            if done(self) {
                return Ok(());
            }
        }
        panic!(
            "never reached: {what}; steps {:?}, jobs {:?}, exceptions {}",
            self.store.steps_of(NEED, 1),
            self.jobs.jobs.keys().collect::<Vec<_>>(),
            self.exceptions()
        );
    }

    fn exceptions(&self) -> String {
        std::fs::read_to_string(self.ctx.home.join("exceptions.txt")).unwrap_or_default()
    }

    /// The `flow-need-locked` lines of the exceptions log.
    fn lock_alarms(&self) -> Vec<String> {
        self.exceptions()
            .lines()
            .filter(|line| line.contains("flow-need-locked"))
            .map(str::to_owned)
            .collect()
    }

    fn runs(&self) -> Result<Vec<RunRow>, VivariumError> {
        self.store.runs_of(NEED)
    }

    fn row(&self, run: u32, node: &str) -> Option<StepRow> {
        self.store
            .steps_of(NEED, run)
            .ok()?
            .into_iter()
            .find(|row| row.node == node)
    }

    /// The not yet finished job of `node` in run 1.
    fn open_job(&self, node: &str) -> Option<PoolJob> {
        self.open_job_of(1, node)
    }

    /// The not yet finished job of `node` in `run`.
    fn open_job_of(&self, run: u32, node: &str) -> Option<PoolJob> {
        let task = self.row(run, node)?.task;
        self.jobs
            .jobs
            .values()
            .find(|job| job.task.as_deref() == Some(task.as_str()) && !job.status.is_terminal())
            .cloned()
    }

    /// Finish the open job of `node` in run 1 with `receipt`.
    fn finish(&mut self, node: &str, receipt: &str) -> R {
        self.finish_in(1, node, receipt)
    }

    /// Finish the open job of `node` in `run` with `receipt`.
    fn finish_in(&mut self, run: u32, node: &str, receipt: &str) -> R {
        let id = self
            .open_job_of(run, node)
            .ok_or_else(|| VivariumError::Message(format!("no open job for {node}")))?
            .id;
        self.finish_job(&id, receipt)
    }

    /// Finish the job `id` with `receipt`.
    fn finish_job(&mut self, id: &str, receipt: &str) -> R {
        let job = self
            .jobs
            .jobs
            .get_mut(id)
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

    /// Drive run 1 from `rework1` through a blocked second pass to the point
    /// where the cto job is enqueued.
    fn to_cto(&mut self) -> R {
        self.to_cto_with(BLOCK_RANGE, &receipt(0, "all tests passed"))
    }

    /// Like `to_cto`, with the audit2 and test2 receipts given.
    fn to_cto_with(&mut self, audit: &str, test: &str) -> R {
        self.tick_until("the rework1 job", |rig| rig.open_job("rework1").is_some())?;
        self.finish("rework1", &receipt(0, SLICED))?;
        self.tick_until("the audit2 and test2 jobs", |rig| {
            rig.open_job("audit2").is_some() && rig.open_job("test2").is_some()
        })?;
        assert_eq!(
            self.row(1, "rework1").map(|row| row.verdict),
            Some(Some("sliced".to_string()))
        );
        assert_eq!(
            self.row(1, "build2").map(|row| row.verdict),
            Some(Some("built".to_string()))
        );
        self.finish("audit2", &receipt(0, audit))?;
        self.finish("test2", test)?;
        self.tick_until("the cto job", |rig| rig.open_job("cto").is_some())
    }

    fn answer_cto(&mut self, word: &str) -> R {
        self.answer_cto_with(word, "the ruling", None)
    }

    fn answer_cto_with(&mut self, word: &str, reason: &str, new_fact: Option<&str>) -> R {
        let fact = new_fact.map_or_else(String::new, |fact| format!("new_fact = '{fact}'\n"));
        let envelope = format!("+++\nanswer = \"{word}\"\nreason = '{reason}'\n{fact}+++\nruled");
        self.finish("cto", &receipt(0, &envelope))
    }

    /// Finish the open job of `node` in `run` with `receipt` and post the same
    /// result on the step task, as the pool does; later steps read it there.
    fn finish_with_reply(&mut self, run: u32, node: &str, receipt: &str) -> R {
        let task = self
            .row(run, node)
            .ok_or_else(|| VivariumError::Message(format!("no {node} row in run {run}")))?
            .task;
        self.finish_in(run, node, receipt)?;
        self.mail.reply(&task, receipt)
    }
}

#[test]
fn a_second_failed_pass_routes_the_join_to_the_cto() -> R {
    let mut rig = rig("rework1")?;
    rig.to_cto()?;

    // The join is a decision, not a step: its inputs are the answered audit
    // and rung, and its word `block` opened the cto step.
    let verdict = |node: &str| rig.row(1, node).map(|row| (row.state, row.verdict));
    assert_eq!(
        verdict("audit2"),
        Some((StepState::Done, Some("block_range".to_string())))
    );
    assert_eq!(
        verdict("test2"),
        Some((StepState::Done, Some("pass".to_string())))
    );
    let cto = rig
        .open_job("cto")
        .ok_or_else(|| VivariumError::Message("no cto job".into()))?;
    assert_eq!(cto.role, "head-cto");
    assert_eq!(cto.payload["class"], serde_json::json!("cto"));
    assert!(cto.body.contains("need: n1\nruns: 1\n"), "{}", cto.body);
    assert_eq!(
        rig.row(1, "cto").map(|row| row.state),
        Some(StepState::Running)
    );
    assert_eq!(rig.runs()?.len(), 1);
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

/// The answered CTO step ends the run through its restart terminal: the old
/// run is superseded and a new run starts at `entry`.
fn restarts_at(word: &str, entry: &str) -> R {
    let mut rig = rig("rework1")?;
    rig.to_cto()?;
    let cto_task = rig
        .row(1, "cto")
        .ok_or_else(|| VivariumError::Message("no cto row".into()))?
        .task;
    rig.answer_cto(word)?;
    rig.tick_until("the restart", |rig| {
        rig.runs().is_ok_and(|runs| runs.len() == 2)
    })?;

    let runs = rig.runs()?;
    assert_eq!(runs[0].state, "superseded", "{word}");
    assert_eq!(runs[1].run, 2, "{word}");
    assert_eq!(runs[1].state, "live", "{word}");
    assert_eq!(runs[1].entry, entry, "{word}");
    assert!(rig.row(2, entry).is_some(), "{word}: entry step of run 2");
    let cto = rig.row(1, "cto").map(|row| (row.state, row.verdict));
    assert_eq!(cto, Some((StepState::Done, Some(word.to_string()))));
    let task = rig.mail.task_state(&cto_task)?;
    assert_eq!(task.status, TaskStatus::Done, "{word}");
    assert_eq!(task.verdict.as_deref(), Some(word));
    assert!(
        rig.open_job("cto").is_none(),
        "{word}: the cto job is finished"
    );
    Ok(())
}

#[test]
fn a_cto_research_word_restarts_the_need_at_research() -> R {
    restarts_at("research", "research")
}

#[test]
fn a_cto_plan_word_restarts_the_need_at_plan() -> R {
    restarts_at("plan", "plan")
}

#[test]
fn a_cto_slice_word_restarts_the_need_at_slice() -> R {
    restarts_at("slice", "slice")
}

#[test]
fn a_cto_rework_word_restarts_the_need_at_rework1() -> R {
    restarts_at("rework", "rework1")
}

#[test]
fn a_cto_lock_word_opens_the_locked_step_and_it_stays_waiting() -> R {
    let mut rig = rig("rework1")?;
    rig.to_cto()?;
    rig.answer_cto("lock")?;
    rig.tick_until("the locked step", |rig| rig.row(1, "locked").is_some())?;

    let locked = rig.row(1, "locked").map(|row| (row.state, row.verdict));
    assert_eq!(locked, Some((StepState::Waiting, None)));
    assert_eq!(
        rig.row(1, "cto").map(|row| row.verdict),
        Some(Some("lock".to_string()))
    );
    assert_eq!(rig.runs()?.len(), 1);
    assert_eq!(rig.runs()?[0].state, "live");

    // A lock is never polled free: more ticks change nothing.
    rig.tick()?;
    rig.tick()?;
    let locked = rig.row(1, "locked").map(|row| (row.state, row.verdict));
    assert_eq!(locked, Some((StepState::Waiting, None)));
    assert_eq!(rig.runs()?.len(), 1);
    assert_eq!(rig.runs()?[0].state, "live");

    // The lock alarms once, however many ticks it waited.
    let alarms = rig.lock_alarms();
    assert_eq!(alarms.len(), 1, "{alarms:?}");
    for needle in [NEED, "lock", "the ruling", "run count 1"] {
        assert!(
            alarms[0].contains(needle),
            "{needle} missing: {}",
            alarms[0]
        );
    }
    Ok(())
}

#[test]
fn a_failed_cto_reaches_the_lock_and_alarms_once_with_failed() -> R {
    let mut rig = rig("rework1")?;
    rig.to_cto()?;
    // An envelope that never parses: two corrections, then `failed`. A
    // correction job belongs to no task, so it is finished by its id.
    let cto_task = rig
        .row(1, "cto")
        .ok_or_else(|| VivariumError::Message("no cto step".into()))?
        .task;
    rig.finish("cto", &receipt(0, "no envelope here"))?;
    for n in 1..=2 {
        let id = format!("step-{cto_task}-a1-c{n}");
        rig.tick_until("the correction job", |rig| rig.jobs.jobs.contains_key(&id))?;
        rig.finish_job(&id, &receipt(0, "still no envelope"))?;
    }
    rig.tick_until("the locked step", |rig| rig.row(1, "locked").is_some())?;
    rig.tick()?;
    rig.tick()?;

    let alarms = rig.lock_alarms();
    assert_eq!(alarms.len(), 1, "{alarms:?}");
    for needle in [NEED, "locked", "failed", "no usable CTO envelope"] {
        assert!(
            alarms[0].contains(needle),
            "{needle} missing: {}",
            alarms[0]
        );
    }
    Ok(())
}

#[test]
fn a_cto_rework_restart_briefs_rework1_and_audit2_from_run_one() -> R {
    let mut rig = rig("rework1")?;
    // A merged slice of run 1, as the first pass left it.
    let slice_task = rig.mail.send_step(
        "mind",
        "mind",
        "slice a",
        &step_body(NEED, 1, "slice:a", ""),
        &[],
    )?;
    rig.mail.close_task(&slice_task, "merged", None, None)?;
    rig.store.insert_step(
        NEED,
        &StepRow {
            run: 1,
            node: slice_node("a"),
            task: slice_task,
            state: StepState::Done,
            verdict: Some("merged".into()),
        },
    )?;
    let failing = receipt(1, &format!("test {FAILING_TEST} ... FAILED"));
    rig.to_cto_with(BLOCK_WITH_FINDING, &failing)?;
    rig.answer_cto_with("rework", "R-why", Some("X-fact"))?;
    rig.tick_until("run 2's rework1 job", |rig| {
        rig.open_job_of(2, "rework1").is_some()
    })?;

    let job = rig
        .open_job_of(2, "rework1")
        .ok_or_else(|| VivariumError::Message("no run 2 rework1 job".into()))?;
    for needle in [
        "F-1",
        FAILING_TEST,
        &slice_node("a"),
        "R-why",
        "X-fact",
        "context: run 1",
    ] {
        assert!(job.body.contains(needle), "{needle} missing: {}", job.body);
    }

    rig.finish_in(2, "rework1", &receipt(0, SLICED))?;
    rig.tick_until("run 2's audit2 job", |rig| {
        rig.open_job_of(2, "audit2").is_some()
    })?;
    let audit = rig
        .open_job_of(2, "audit2")
        .ok_or_else(|| VivariumError::Message("no run 2 audit2 job".into()))?;
    assert!(
        audit.body.contains("## Second pass: prior audit"),
        "{}",
        audit.body
    );
    assert!(audit.body.contains("F-1"), "{}", audit.body);
    Ok(())
}

/// The stored payload body of `node`'s step in `run`: what an admission reads.
fn stored_body(rig: &Rig, run: u32, node: &str) -> Result<String, VivariumError> {
    let task = rig
        .row(run, node)
        .ok_or_else(|| VivariumError::Message(format!("no run {run} {node} row")))?
        .task;
    let row = rig
        .store
        .step_payload(&task)?
        .ok_or_else(|| VivariumError::Message(format!("{node} has no stored payload")))?;
    row.payload["body"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| VivariumError::Message(format!("{node} payload has no body")))
}

/// Drive the fresh `plan` run through a real admission to its own slicer job:
/// `revised` takes the plan2/admit2 revision, else plan/admit1. Returns the
/// slicer job, whose brief already carries the stored plan.
fn to_own_slicer(rig: &mut Rig, revised: bool) -> Result<PoolJob, VivariumError> {
    rig.tick_until("the plan job", |rig| rig.open_job("plan").is_some())?;
    rig.finish_with_reply(1, "plan", &receipt(0, PLANNED_ONE))?;
    rig.tick_until("the admit1 job", |rig| rig.open_job("admit1").is_some())?;
    if revised {
        rig.finish_with_reply(1, "admit1", &receipt(0, REVISE))?;
        rig.tick_until("the plan2 job", |rig| rig.open_job("plan2").is_some())?;
        rig.finish_with_reply(1, "plan2", &receipt(0, PLANNED_TWO))?;
        rig.tick_until("the admit2 job", |rig| rig.open_job("admit2").is_some())?;
        rig.finish_with_reply(1, "admit2", &receipt(0, ADMITTED))?;
    } else {
        rig.finish_with_reply(1, "admit1", &receipt(0, ADMITTED))?;
    }
    rig.tick_until("the first slicer job", |rig| {
        rig.open_job("slice").is_some()
    })?;
    rig.open_job("slice")
        .ok_or_else(|| VivariumError::Message("no slicer job".into()))
}

/// The run's slicer escalates and the CTO rules `slice`: run 1 is superseded
/// and run 2 is live at the slice entry.
fn restart_at_slice(rig: &mut Rig) -> R {
    rig.finish_in(1, "slice", &receipt(0, ESCALATE))?;
    rig.tick_until("the cto job", |rig| rig.open_job("cto").is_some())?;
    rig.answer_cto("slice")?;
    rig.tick_until("the restart", |rig| {
        rig.runs().is_ok_and(|runs| runs.len() == 2)
    })?;
    let runs = rig.runs()?;
    assert_eq!(runs[0].state, "superseded");
    assert_eq!((runs[1].run, runs[1].entry.as_str()), (2, "slice"));
    assert_eq!(runs[1].state, "live");
    let cto = rig
        .row(1, "cto")
        .ok_or_else(|| VivariumError::Message("no run 1 cto row".into()))?;
    assert_eq!(
        (cto.state, cto.verdict),
        (StepState::Done, Some("slice".into()))
    );
    Ok(())
}

/// Run 2's real fanout sent the declared slices `a` and `b` from the
/// slicer's posted reply, bound to the need, with the verify pass waiting.
fn assert_fanned_out(rig: &Rig, slicer_task: &str) -> R {
    let task = |title: &str| {
        rig.mail
            .tasks
            .iter()
            .find(|(_, task)| task.subject == format!("{title} ({NEED} r2)"))
            .map(|(handle, _)| handle.clone())
            .ok_or_else(|| VivariumError::Message(format!("no slice task {title}")))
    };
    let a = task("first")?;
    let b = task("second")?;
    let row = |node: &str| {
        rig.row(2, node)
            .ok_or_else(|| VivariumError::Message(format!("no run 2 {node} row")))
    };
    let sliced = row("slice")?;
    assert_eq!(
        (sliced.state, sliced.verdict),
        (StepState::Done, Some("sliced".into()))
    );
    for (node, handle) in [("slice:a", &a), ("slice:b", &b)] {
        let row = row(node)?;
        assert_eq!(row.state, StepState::Pending, "{node}");
        assert_eq!(row.task, *handle, "{node}");
    }
    // The second slice waits on the first; both wait on the slicer.
    assert_eq!(rig.mail.tasks[&a].depends_on, vec![slicer_task.to_string()]);
    assert_eq!(
        rig.mail.tasks[&b].depends_on,
        vec![slicer_task.to_string(), a.clone()]
    );
    // The declared write scopes landed and the units are bound to the need.
    assert_eq!(
        rig.store.slice_scope(&a)?,
        Some(vec![String::from("src/a.rs")])
    );
    let bound = rig.mail.bound_units(NEED)?;
    assert!(bound.contains(&a), "{bound:?}");
    assert!(bound.contains(&b), "{bound:?}");
    // The fanout itself stays open: unrelated later work does not run.
    let build1 = row("build1")?;
    assert_eq!((build1.state, build1.verdict), (StepState::Running, None));
    assert!(rig.row(2, "audit1").is_none(), "the verify pass must wait");
    assert!(rig.row(2, "test1").is_none(), "the verify pass must wait");
    Ok(())
}

/// A CTO `slice` ruling restarts the need onto the admitted plan: run 2's
/// real slicer inherits the stored plan body, answers `sliced`, and the real
/// fanout expands the declared slices. Both the initial and the revised
/// admission are exercised; the slicer and the fanout are the shipped ones.
#[test]
fn a_cto_slice_restart_slices_the_admitted_plan_and_fans_out() -> R {
    for (revised, plan_node, plan, dropped) in [
        (false, "plan", "PLAN-ONE BODY", "PLAN-TWO BODY"),
        (true, "plan2", "PLAN-TWO BODY", "PLAN-ONE BODY"),
    ] {
        let mut rig = plan_rig()?;
        let slicer = to_own_slicer(&mut rig, revised)?;
        assert_eq!(stored_body(&rig, 1, plan_node)?, plan);
        assert!(
            slicer.body.contains(&format!("admitted plan:\n{plan}")),
            "{}",
            slicer.body
        );
        restart_at_slice(&mut rig)?;

        // Run 2's actual slicer executes on the inherited admission.
        rig.tick_until("run 2's slicer job", |rig| {
            rig.open_job_of(2, "slice").is_some()
        })?;
        let slicer = rig
            .open_job_of(2, "slice")
            .ok_or_else(|| VivariumError::Message("no run 2 slicer job".into()))?;
        assert!(
            slicer.body.contains("need: n1\nrun: 2\n"),
            "{}",
            slicer.body
        );
        assert!(
            slicer.body.contains(&format!("admitted plan:\n{plan}")),
            "{}",
            slicer.body
        );
        assert!(!slicer.body.contains(dropped), "{}", slicer.body);
        let slicer_task = rig
            .row(2, "slice")
            .ok_or_else(|| VivariumError::Message("no run 2 slicer row".into()))?
            .task;

        rig.finish_with_reply(2, "slice", &receipt(0, SLICED_PLAN))?;
        rig.tick_until("the declared slices", |rig| {
            rig.row(2, "slice:a").is_some() && rig.row(2, "slice:b").is_some()
        })?;
        assert_fanned_out(&rig, &slicer_task)?;
        // No hand role is routed here, so the pump cannot dispatch the slices
        // it just created; that configuration gap is the only alarm.
        for line in rig.exceptions().lines() {
            assert!(line.contains("flow-run-error"), "{line}");
            assert!(line.contains("no hand role"), "{line}");
        }
    }
    Ok(())
}
