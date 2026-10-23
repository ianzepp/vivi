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
use crate::pump::config::{PumpConfig, RungsConfig};
use crate::pump::flow::advance::start::start;
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::{Registry, shipped};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::mail::{MailReader, MailWriter, TaskStatus};
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::tick::tick_with;
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::jobs::{JobOutcome, JobStatus, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};

type R = Result<(), VivariumError>;

const NEED: &str = "n1";
const BASE: &str = "0123456789abcdef0123456789abcdef01234567";
/// Ticks a stage may take before the test gives up.
const MAX_TICKS: usize = 8;

const SLICED: &str = "+++\nanswer = \"sliced\"\nmode = \"rework\"\n\n[[slice]]\nid = \"fix-1\"\n\
                      title = 'Fix it'\nbrief = 'fix the finding'\ntarget = \"default\"\n\
                      write_scope = [\"src/a.rs\"]\n+++\ncut one rework slice";
const BLOCK_RANGE: &str = "+++\nanswer = \"block_range\"\n+++\nthe range is still broken";

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
        "need show",
        0,
        &serde_json::json!([
            {"handle": NEED, "message_id": "m1", "from": "operator@local",
             "subject": "Need", "body": "Build the thing"}
        ])
        .to_string(),
    );
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
    start(&ctx, &store, &mut mail, NEED, Some(entry))?;
    Ok(Rig {
        _dir: dir,
        ctx,
        store,
        mail,
        jobs: Jobs::default(),
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
    fn registry(&self) -> Registry<Jobs, FakeMail> {
        let mut registry = shipped(&self.ctx);
        registry.handler(FlowKind::Build, NodeClass::Fanout, Box::new(BuiltAtOnce));
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
        let task = self.row(1, node)?.task;
        self.jobs
            .jobs
            .values()
            .find(|job| job.task.as_deref() == Some(task.as_str()) && !job.status.is_terminal())
            .cloned()
    }

    /// Finish the open job of `node` with `receipt`.
    fn finish(&mut self, node: &str, receipt: &str) -> R {
        let id = self
            .open_job(node)
            .ok_or_else(|| VivariumError::Message(format!("no open job for {node}")))?
            .id;
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

    /// Drive run 1 from `rework1` through a blocked second pass to the point
    /// where the cto job is enqueued.
    fn to_cto(&mut self) -> R {
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
        self.finish("audit2", &receipt(0, BLOCK_RANGE))?;
        self.finish("test2", &receipt(0, "all tests passed"))?;
        self.tick_until("the cto job", |rig| rig.open_job("cto").is_some())
    }

    fn answer_cto(&mut self, word: &str) -> R {
        let envelope = format!("+++\nanswer = \"{word}\"\nreason = 'the ruling'\n+++\nruled");
        self.finish("cto", &receipt(0, &envelope))
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
    assert_eq!(rig.exceptions(), "");
    Ok(())
}
