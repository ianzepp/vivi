//! The repair merge-main collector: receipts, git-settled lost jobs and the
//! environment hold.

use std::collections::BTreeMap;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::super::register;
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::flow::advance::start::start;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{StepJobRow, StepRow, StepState};
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::main_rung;
use crate::pump::flow::tick::{OPERATOR_HOLD, tick_with};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobOutcome, JobStatus, JobStore, PoolJob, merge_main};
use crate::pump::store::{DB_FILENAME, PumpStore};
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

const NEED: &str = "n1";
const NODE: &str = "merge1";
const NEED_TIP: &str = "3333333333333333333333333333333333333333";
const MAIN_A: &str = "1111111111111111111111111111111111111111";
const MAIN_B: &str = "4444444444444444444444444444444444444444";
const MERGED: &str = "2222222222222222222222222222222222222222";

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
    registry: Registry<Jobs, FakeMail>,
    task: String,
}

/// A repair run whose `merge1` step is Running with its merge-main job (cut
/// from `MAIN_A`) in the job store.
fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let mut routing = (*ctx.routing).clone();
    routing.roles.push(RoutedRole {
        name: String::from("merge"),
        kind: Some(String::from("merge")),
        status: String::from("active"),
        pool: Some(String::from("test-planner-pool")),
        read_only: Some(false),
        model: None,
        models: Vec::new(),
    });
    let ctx = ctx.with_routing(routing);
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("mind");
    MailWriter::open_record(&mut mail, NEED, "kind: repair\ntarget: default")?;
    start(&ctx, &store, &mut mail, NEED, Some("fix1"))?;
    let task = mail.send_step(
        "mind",
        "mind",
        NODE,
        &format!("pump-step: {NEED} r1 {NODE}"),
        &[],
    )?;
    store.insert_step(
        NEED,
        &StepRow {
            run: 1,
            node: NODE.into(),
            task: task.clone(),
            state: StepState::Running,
            verdict: None,
        },
    )?;
    let mut registry = Registry::new();
    register(&mut registry);
    let mut rig = Rig {
        _dir: dir,
        ctx,
        host,
        store,
        mail,
        jobs: Jobs::default(),
        registry,
        task,
    };
    rig.seed()?;
    Ok(rig)
}

impl Rig {
    /// The job the merge handler and the tick would have made.
    fn seed(&mut self) -> R {
        let spec = merge_main::spec(&self.ctx, "default", NEED, NEED_TIP, MAIN_A, &self.task)
            .ok_or_else(|| VivariumError::Message("no merge route".into()))?;
        let mut spec = merge_main::with_attempt(spec, "default", 1);
        spec.task = Some(self.task.clone());
        let mut payload = spec.payload.clone();
        if let Some(map) = payload.as_object_mut() {
            map.insert("run".into(), serde_json::json!(1));
            map.insert("node".into(), serde_json::json!(NODE));
            map.insert("task".into(), serde_json::json!(self.task));
            map.insert("attempt".into(), serde_json::json!(1));
            map.insert("kind".into(), serde_json::json!("repair"));
            map.insert("class".into(), serde_json::json!("merge-main"));
        }
        spec.payload = payload;
        let id = spec.id.clone();
        crate::pump::jobs::enqueue(&mut self.jobs, spec)?;
        self.store.insert_step_job(
            NEED,
            &StepJobRow {
                job: id,
                run: 1,
                node: NODE.into(),
                task: self.task.clone(),
                attempt: 1,
            },
        )?;
        Ok(())
    }

    fn job_id(&self) -> String {
        format!("merge-main-{}-{}-a1", self.task, &MAIN_A[..12])
    }

    fn finish(&mut self, status: JobStatus, receipt: &str) -> R {
        let id = self.job_id();
        let job = self
            .jobs
            .jobs
            .get_mut(&id)
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
            .step_by_task(&self.task)?
            .ok_or_else(|| VivariumError::Message("no merge step".into()))
    }

    fn main_is(&self, sha: &str) {
        self.host
            .reply("ls-remote", 0, &format!("{sha}\trefs/heads/main\n"));
    }

    /// The word the step answered and the reason its payload recorded.
    fn answered(&self, word: &str) -> Result<String, VivariumError> {
        let step = self.step()?;
        assert_eq!(step.state, StepState::Done);
        assert_eq!(step.verdict.as_deref(), Some(word));
        assert_eq!(self.jobs.jobs.len(), 1, "nothing is queued again");
        let row = self
            .store
            .step_payload(&self.task)?
            .ok_or_else(|| VivariumError::Message("no step payload".into()))?;
        Ok(row.payload["reason"]
            .as_str()
            .unwrap_or_default()
            .to_owned())
    }

    /// The recorded `main-repaired:default` value.
    fn repaired(&self) -> Result<Option<String>, VivariumError> {
        self.store
            .main_state(&main_rung::key(main_rung::REPAIRED, "default"))
    }

    fn exceptions(&self) -> String {
        std::fs::read_to_string(self.ctx.home.join("exceptions.txt")).unwrap_or_default()
    }
}

#[test]
fn a_merged_receipt_answers_merged() -> R {
    let mut rig = rig()?;
    rig.finish(
        JobStatus::Finished,
        &format!("agent exit 0 in 1s\nrunner publish: pushed refs/heads/main {MERGED} {MERGED}"),
    )?;
    rig.tick()?;
    let step = rig.step()?;
    assert_eq!(step.state, StepState::Done);
    assert_eq!(step.verdict.as_deref(), Some("merged"));
    assert_eq!(rig.repaired()?.as_deref(), Some(MERGED));
    Ok(())
}

#[test]
fn a_conflict_receipt_answers_conflict() -> R {
    let mut rig = rig()?;
    rig.finish(
        JobStatus::Finished,
        "CONFLICT (content): merge conflict in a.rs\nagent exit 1 in 1s",
    )?;
    rig.tick()?;
    assert_eq!(rig.answered("conflict")?, "merge conflict");
    assert_eq!(rig.repaired()?, None);
    Ok(())
}

#[test]
fn a_not_ancestor_receipt_answers_conflict_for_a_moved_main() -> R {
    let mut rig = rig()?;
    rig.finish(
        JobStatus::Finished,
        &format!("agent exit 0 in 1s\nrunner publish: not_ancestor refs/heads/main {MERGED} "),
    )?;
    rig.tick()?;
    assert_eq!(rig.answered("conflict")?, "main moved");
    assert_eq!(rig.repaired()?, None);
    Ok(())
}

#[test]
fn a_lost_job_whose_main_holds_the_tip_records_the_repair_commit_not_the_later_main() -> R {
    let mut rig = rig()?;
    rig.main_is(MAIN_B);
    rig.host.reply("cat-file", 0, "");
    rig.host.reply("merge-base", 0, "");
    rig.finish(JobStatus::Lost, "")?;
    rig.tick()?;
    let step = rig.step()?;
    assert_eq!(step.state, StepState::Done);
    assert_eq!(step.verdict.as_deref(), Some("merged"));
    assert_eq!(rig.jobs.jobs.len(), 1);
    assert_eq!(rig.repaired()?.as_deref(), Some(NEED_TIP));
    Ok(())
}

#[test]
fn a_lost_job_on_a_moved_main_without_the_tip_answers_conflict() -> R {
    let mut rig = rig()?;
    rig.main_is(MAIN_B);
    rig.host.reply("cat-file", 128, "");
    rig.finish(JobStatus::Lost, "")?;
    rig.tick()?;
    let reason = rig.answered("conflict")?;
    assert_eq!(
        reason,
        format!("main moved from {MAIN_A} to {MAIN_B} without {NEED_TIP}")
    );
    assert_eq!(rig.repaired()?, None);
    Ok(())
}

#[test]
fn a_lost_job_with_main_unmoved_answers_failed() -> R {
    let mut rig = rig()?;
    rig.main_is(MAIN_A);
    rig.host.reply("cat-file", 128, "");
    rig.finish(JobStatus::Lost, "")?;
    rig.tick()?;
    let reason = rig.answered("failed")?;
    assert_eq!(
        reason,
        format!("expected main to contain {NEED_TIP}; actual: main {MAIN_A} without it")
    );
    assert_eq!(rig.repaired()?, None);
    Ok(())
}

#[test]
fn a_missing_tool_holds_the_step_with_one_alarm_and_no_word() -> R {
    let mut rig = rig()?;
    rig.finish(
        JobStatus::Finished,
        "agent exit 127 in 1s\n\n--- stderr tail\nbash: git: command not found\n",
    )?;
    for _ in 0..2 {
        rig.tick()?;
        let step = rig.step()?;
        assert_eq!(step.state, StepState::Waiting);
        assert_eq!(step.verdict.as_deref(), Some(OPERATOR_HOLD));
        assert_eq!(rig.jobs.jobs.len(), 1);
    }
    let text = rig.exceptions();
    assert_eq!(text.matches("flow-env-failure").count(), 1, "{text}");
    assert!(text.contains("expected git to run on the worker"), "{text}");
    assert!(text.contains("tool git"), "{text}");
    assert!(rig.store.step_payload(&rig.task)?.is_none());
    Ok(())
}
