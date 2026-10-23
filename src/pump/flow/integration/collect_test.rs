//! The build merge-main collector: moved-main retries, conflicts and the lost
//! rule.

use std::collections::BTreeMap;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::BuildMergeMain;
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::flow::advance::start::start;
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{StepJobRow, StepRow, StepState};
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::tick::{OPERATOR_HOLD, tick_with};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobOutcome, JobStatus, JobStore, PoolJob, merge_main};
use crate::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

const NEED: &str = "n1";
const NODE: &str = "merge";
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

fn with_merge_route(ctx: Ctx) -> Ctx {
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
    ctx.with_routing(routing)
}

/// A build run whose `merge` step is Running with a merge-main job of attempt
/// `attempt` in the job store.
fn rig(attempt: u32) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let ctx = with_merge_route(ctx);
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("mind");
    MailWriter::open_record(&mut mail, NEED, "kind: build")?;
    start(&ctx, &store, &mut mail, NEED, Some("plan"))?;
    let task = mail.send_step(
        "mind",
        "mind",
        "merge",
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
    registry.collector(
        FlowKind::Build,
        NodeClass::MergeMain,
        Box::new(BuildMergeMain),
    );
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
    rig.seed(attempt)?;
    Ok(rig)
}

impl Rig {
    /// The job the start handler and the tick would have made for `attempt`.
    fn seed(&mut self, attempt: u32) -> R {
        let spec = merge_main::spec(&self.ctx, "default", NEED, NEED_TIP, MAIN_A, &self.task)
            .ok_or_else(|| VivariumError::Message("no merge route".into()))?;
        let mut spec = merge_main::with_attempt(spec, "default", attempt);
        spec.task = Some(self.task.clone());
        let mut payload = spec.payload.clone();
        if let Some(map) = payload.as_object_mut() {
            map.insert("run".into(), serde_json::json!(1));
            map.insert("node".into(), serde_json::json!(NODE));
            map.insert("task".into(), serde_json::json!(self.task));
            map.insert("attempt".into(), serde_json::json!(attempt));
            map.insert("kind".into(), serde_json::json!("build"));
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
                attempt,
            },
        )?;
        Ok(())
    }

    fn job_id(&self, attempt: u32) -> String {
        format!("merge-main-{}-{}-a{attempt}", self.task, &MAIN_A[..12])
    }

    fn finish(&mut self, attempt: u32, status: JobStatus, receipt: &str) -> R {
        let id = self.job_id(attempt);
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
}

fn pushed() -> String {
    format!("agent exit 0 in 1s\nrunner publish: pushed refs/heads/main {MERGED} {MERGED}")
}

fn moved() -> String {
    format!("agent exit 0 in 1s\nrunner publish: not_ancestor refs/heads/main {MERGED} ")
}

#[test]
fn a_merged_receipt_answers_merged() -> R {
    let mut rig = rig(1)?;
    rig.finish(1, JobStatus::Finished, &pushed())?;
    rig.tick()?;
    let step = rig.step()?;
    assert_eq!(step.state, StepState::Done);
    assert_eq!(step.verdict.as_deref(), Some("merged"));
    assert_eq!(rig.jobs.jobs.len(), 1);
    Ok(())
}

#[test]
fn a_moved_main_on_attempt_one_queues_attempt_two_at_the_new_tip() -> R {
    let mut rig = rig(1)?;
    rig.main_is(MAIN_B);
    rig.finish(1, JobStatus::Finished, &moved())?;
    rig.tick()?;
    let id = format!("merge-main-{}-{}-a2", rig.task, &MAIN_B[..12]);
    let again = rig.jobs.jobs.get(&id).ok_or_else(|| {
        VivariumError::Message(format!("no job {id}: {:?}", rig.jobs.jobs.keys()))
    })?;
    assert_eq!(again.from_ref.as_deref(), Some(MAIN_B));
    assert_eq!(again.task.as_deref(), Some(rig.task.as_str()));
    assert_eq!(again.payload["attempt"], serde_json::json!(2));
    assert_eq!(again.payload["main_tip"], serde_json::json!(MAIN_B));
    assert_eq!(again.payload["need_tip"], serde_json::json!(NEED_TIP));
    assert_eq!(again.payload["kind"], serde_json::json!("build"));
    assert_eq!(again.payload["class"], serde_json::json!("merge-main"));
    assert_eq!(rig.step()?.state, StepState::Running);
    assert_eq!(rig.store.attempts_of(&rig.task)?, 2);
    let stored = rig
        .store
        .step_job(&id)?
        .ok_or_else(|| VivariumError::Message("no step_jobs row".into()))?;
    assert_eq!(stored.attempt, 2);
    assert_eq!(again.payload["attempt"], serde_json::json!(stored.attempt));
    assert_eq!(again.payload["run"], serde_json::json!(stored.run));
    assert_eq!(again.payload["node"], serde_json::json!(stored.node));
    assert_eq!(again.payload["task"], serde_json::json!(stored.task));
    Ok(())
}

#[test]
fn a_moved_main_on_attempt_four_answers_conflict() -> R {
    let mut rig = rig(4)?;
    rig.main_is(MAIN_B);
    rig.finish(4, JobStatus::Finished, &moved())?;
    rig.tick()?;
    assert_eq!(rig.jobs.jobs.len(), 1);
    let step = rig.step()?;
    assert_eq!(step.state, StepState::Done);
    assert_eq!(step.verdict.as_deref(), Some("conflict"));
    Ok(())
}

#[test]
fn a_real_conflict_answers_conflict_on_attempt_one() -> R {
    let mut rig = rig(1)?;
    rig.finish(
        1,
        JobStatus::Finished,
        "CONFLICT (content): merge conflict in a.rs\nagent exit 1 in 1s",
    )?;
    rig.tick()?;
    assert_eq!(rig.jobs.jobs.len(), 1);
    assert_eq!(rig.step()?.verdict.as_deref(), Some("conflict"));
    Ok(())
}

#[test]
fn a_lost_job_whose_merge_main_already_holds_answers_merged() -> R {
    let mut rig = rig(1)?;
    rig.main_is(MAIN_B);
    rig.host.reply("cat-file", 0, "");
    rig.host.reply("merge-base", 0, "");
    rig.finish(1, JobStatus::Lost, "")?;
    rig.tick()?;
    assert_eq!(rig.jobs.jobs.len(), 1);
    let step = rig.step()?;
    assert_eq!(step.state, StepState::Done);
    assert_eq!(step.verdict.as_deref(), Some("merged"));
    Ok(())
}

#[test]
fn a_lost_job_whose_need_tip_is_the_run_base_answers_failed_never_merged() -> R {
    let mut rig = rig(1)?;
    rig.store.upsert_need_branch(&NeedBranchRow {
        need: NEED.into(),
        branch: format!("vivi/need/{NEED}"),
        base: Some(NEED_TIP.into()),
        status: "live".into(),
        run: 1,
        target: "default".into(),
        priors: Vec::new(),
    })?;
    // The host would say main holds the tip: it is the base, so it always does.
    rig.main_is(MAIN_B);
    rig.host.reply("cat-file", 0, "");
    rig.host.reply("merge-base", 0, "");
    rig.finish(1, JobStatus::Lost, "")?;
    rig.tick()?;
    assert_eq!(rig.jobs.jobs.len(), 1);
    let step = rig.step()?;
    assert_eq!(step.verdict.as_deref(), Some("failed"));
    let stored = rig.store.step_payload(&rig.task)?.expect("payload row");
    let reason = stored.payload["reason"].as_str().unwrap_or_default();
    assert!(reason.contains(NEED_TIP) && reason.contains("nothing to merge"));
    Ok(())
}

#[test]
fn a_failed_job_payload_names_expected_and_actual() -> R {
    let mut rig = rig(1)?;
    rig.finish(
        1,
        JobStatus::Finished,
        "fatal: bad ref\nagent exit 128 in 1s",
    )?;
    rig.tick()?;
    let stored = rig.store.step_payload(&rig.task)?.expect("payload row");
    let payload = &stored.payload;
    assert_eq!(payload["job"], serde_json::json!(rig.job_id(1)));
    assert_eq!(payload["need_tip"], serde_json::json!(NEED_TIP));
    assert_eq!(payload["main_tip"], serde_json::json!(MAIN_A));
    let reason = payload["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains(&format!("expected main to take {NEED_TIP}")),
        "{reason}"
    );
    assert!(
        reason.contains("actual: merge-main finished with publish none"),
        "{reason}"
    );
    Ok(())
}

#[test]
fn a_lost_job_with_main_unchanged_queues_again() -> R {
    let mut rig = rig(1)?;
    rig.main_is(MAIN_A);
    rig.host.reply("cat-file", 128, "");
    rig.finish(1, JobStatus::Lost, "")?;
    rig.tick()?;
    assert!(
        rig.jobs.jobs.contains_key(&rig.job_id(2)),
        "{:?}",
        rig.jobs.jobs.keys()
    );
    assert_eq!(rig.step()?.state, StepState::Running);
    Ok(())
}

#[test]
fn a_lost_job_with_main_unchanged_on_attempt_four_answers_failed() -> R {
    let mut rig = rig(4)?;
    rig.main_is(MAIN_A);
    rig.host.reply("cat-file", 128, "");
    rig.finish(4, JobStatus::Timeout, "")?;
    rig.tick()?;
    assert_eq!(rig.jobs.jobs.len(), 1);
    assert_eq!(rig.step()?.verdict.as_deref(), Some("failed"));
    Ok(())
}

#[test]
fn a_lost_job_on_a_moved_main_without_the_need_tip_queues_again() -> R {
    let mut rig = rig(1)?;
    rig.main_is(MAIN_B);
    rig.host.reply("cat-file", 128, "");
    rig.finish(1, JobStatus::Lost, "")?;
    rig.tick()?;
    let id = format!("merge-main-{}-{}-a2", rig.task, &MAIN_B[..12]);
    assert!(
        rig.jobs.jobs.contains_key(&id),
        "{:?}",
        rig.jobs.jobs.keys()
    );
    assert_eq!(rig.step()?.state, StepState::Running);
    Ok(())
}

#[test]
fn a_lost_job_whose_need_tip_is_not_an_ancestor_of_main_queues_again() -> R {
    let mut rig = rig(1)?;
    rig.main_is(MAIN_B);
    rig.host.reply("cat-file", 0, "");
    rig.host.reply("merge-base", 1, "");
    rig.finish(1, JobStatus::Lost, "")?;
    rig.tick()?;
    let id = format!("merge-main-{}-{}-a2", rig.task, &MAIN_B[..12]);
    assert!(
        rig.jobs.jobs.contains_key(&id),
        "{:?}",
        rig.jobs.jobs.keys()
    );
    assert_eq!(rig.step()?.state, StepState::Running);
    Ok(())
}

#[test]
fn a_lost_job_with_a_failing_fetch_holds_the_step_with_one_alarm() -> R {
    let mut rig = rig(1)?;
    rig.main_is(MAIN_B);
    rig.host.reply("cat-file", 128, "");
    rig.host.reply("fetch", 1, "network down");
    rig.finish(1, JobStatus::Lost, "")?;
    rig.tick()?;
    held(&rig)?;
    let text = exceptions(&rig);
    assert_eq!(
        text.matches("flow-side-effect-unclear").count(),
        1,
        "{text}"
    );
    rig.tick()?;
    held(&rig)?;
    assert_eq!(
        exceptions(&rig).matches("flow-side-effect-unclear").count(),
        1
    );
    Ok(())
}

fn exceptions(rig: &Rig) -> String {
    std::fs::read_to_string(rig.ctx.home.join("exceptions.txt")).unwrap_or_default()
}

fn held(rig: &Rig) -> R {
    let step = rig.step()?;
    assert_eq!(step.state, StepState::Waiting);
    assert_eq!(step.verdict.as_deref(), Some(OPERATOR_HOLD));
    assert_eq!(rig.jobs.jobs.len(), 1);
    Ok(())
}

#[test]
fn a_missing_tool_alarms_once_and_holds_the_step() -> R {
    let mut rig = rig(1)?;
    rig.finish(
        1,
        JobStatus::Finished,
        "agent exit 127 in 1s\n\n--- stderr tail\nbash: git: command not found\n",
    )?;
    rig.tick()?;
    held(&rig)?;
    let text = exceptions(&rig);
    assert_eq!(text.matches("flow-env-failure").count(), 1, "{text}");
    assert!(text.contains("tool git"), "{text}");
    rig.tick()?;
    held(&rig)?;
    assert_eq!(exceptions(&rig).matches("flow-env-failure").count(), 1);
    Ok(())
}

#[test]
fn a_failed_checkout_holds_the_step() -> R {
    let mut rig = rig(1)?;
    rig.finish(1, JobStatus::Finished, "agent exit 1 in 1s")?;
    let id = rig.job_id(1);
    if let Some(outcome) = rig.jobs.jobs.get_mut(&id).and_then(|j| j.outcome.as_mut()) {
        outcome.detail = "checkout_failed exited 128".into();
    }
    rig.tick()?;
    held(&rig)?;
    assert_eq!(exceptions(&rig).matches("flow-env-failure").count(), 1);
    Ok(())
}

#[test]
fn an_already_on_remote_receipt_answers_merged() -> R {
    let mut rig = rig(1)?;
    let receipt = format!(
        "agent exit 0 in 1s\nrunner publish: already-on-remote refs/heads/main {MERGED} {MERGED}"
    );
    rig.finish(1, JobStatus::Finished, &receipt)?;
    rig.tick()?;
    let step = rig.step()?;
    assert_eq!(step.state, StepState::Done);
    assert_eq!(step.verdict.as_deref(), Some("merged"));
    assert_eq!(rig.jobs.jobs.len(), 1);
    Ok(())
}

#[test]
fn a_setup_error_answers_failed() -> R {
    let mut rig = rig(1)?;
    rig.finish(
        1,
        JobStatus::Finished,
        "fatal: bad ref\nagent exit 128 in 1s",
    )?;
    rig.tick()?;
    assert_eq!(rig.jobs.jobs.len(), 1);
    assert_eq!(rig.step()?.verdict.as_deref(), Some("failed"));
    Ok(())
}
