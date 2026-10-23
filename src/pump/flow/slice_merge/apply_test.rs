//! Tests for applying a finished slice merge job.

use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::{FAILED_MARKER, run};
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::flow::fake_mail::{Call, FakeMail};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::slice_dispatch;
use crate::pump::flow::slice_merge::spec::KIND;
use crate::pump::flow::tick::OPERATOR_HOLD;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobOutcome, JobSpec, JobStatus, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

const NEED: &str = "n1";
const BEFORE: &str = "1111111111111111111111111111111111111111";
const HAND: &str = "2222222222222222222222222222222222222222";
const TIP: &str = "3333333333333333333333333333333333333333";

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

fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let mut routing = (*ctx.routing).clone();
    routing.roles.push(RoutedRole {
        name: String::from("hand"),
        kind: Some(String::from("hand")),
        status: String::from("active"),
        pool: Some(String::from("test-planner-pool")),
        read_only: None,
        model: None,
        models: Vec::new(),
    });
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    store.insert_run(&RunRow {
        need: NEED.into(),
        run: 1,
        flow: "build".into(),
        flow_version: 1,
        kind: "build".into(),
        entry: "plan".into(),
        state: "live".into(),
    })?;
    store.upsert_need_branch(&NeedBranchRow {
        need: NEED.into(),
        branch: format!("vivi/need/{NEED}"),
        base: Some(BEFORE.into()),
        status: "live".into(),
        run: 1,
        target: "default".into(),
        priors: Vec::new(),
    })?;
    Ok(Rig {
        _dir: dir,
        ctx: ctx.with_routing(routing),
        host,
        store,
        mail: FakeMail::new("pump"),
        jobs: Jobs::default(),
    })
}

impl Rig {
    /// A slice whose first hand attempt was dispatched, and its merge job
    /// with `receipt`. `attempt` is the hand attempt the merge belongs to.
    fn slice(
        &mut self,
        status: JobStatus,
        receipt: &str,
        attempt: u32,
    ) -> Result<StepRow, VivariumError> {
        let task = self.mail.send_step(
            "pump",
            "pump",
            "slice a",
            &format!("pump-step: {NEED} r1 slice:a\ntarget: default\n\ntitle\n\nbrief"),
            &[],
        )?;
        let row = StepRow {
            run: 1,
            node: String::from("slice:a"),
            task: task.clone(),
            state: StepState::Pending,
            verdict: None,
        };
        self.store.insert_step(NEED, &row)?;
        self.store
            .insert_slice_scope(&task, 1, &[String::from("src/a.rs")])?;
        for n in 1..=attempt {
            slice_dispatch::dispatch(
                &self.ctx,
                &mut self.jobs,
                &self.store,
                &self.mail,
                NEED,
                &row,
                n,
            )?;
        }
        self.store.set_step_state(&task, StepState::Running, None)?;
        let mut spec = JobSpec::new(format!("hand-merge-{attempt}"), KIND, "merge", "s", "b");
        spec.task = Some(task.clone());
        spec.target = Some(String::from("default"));
        spec.publish_ref = Some(format!("refs/heads/vivi/need/{NEED}"));
        spec.payload = serde_json::json!({
            "need": NEED,
            "run": 1,
            "node": "slice:a",
            "task": task,
            "attempt": attempt,
            "hand_sha": HAND,
            "before": BEFORE,
        });
        let mut job = PoolJob::from(spec);
        job.status = status;
        job.outcome = Some(JobOutcome {
            state: String::from("finished"),
            detail: String::new(),
            receipt: receipt.to_owned(),
            at: 1,
        });
        self.jobs.jobs.insert(job.id.clone(), job);
        Ok(self.store.step_by_task(&task)?.unwrap_or(row))
    }

    fn apply(&mut self) -> R {
        run(&self.ctx, &mut self.jobs, &self.store, &mut self.mail)
    }

    fn merge_job(&self, attempt: u32) -> &PoolJob {
        &self.jobs.jobs[&format!("hand-merge-{attempt}")]
    }

    fn row(&self, slice: &StepRow) -> Result<StepRow, VivariumError> {
        self.store
            .step_by_task(&slice.task)?
            .ok_or_else(|| VivariumError::Other(String::from("slice row is gone")))
    }

    fn closes(&self) -> usize {
        self.mail
            .calls
            .iter()
            .filter(|call| matches!(call, Call::CloseTask { .. }))
            .count()
    }

    fn replies(&self) -> Vec<&str> {
        self.mail
            .calls
            .iter()
            .filter_map(|call| match call {
                Call::Reply { body, .. } if body.starts_with(FAILED_MARKER) => Some(body.as_str()),
                _ => None,
            })
            .collect()
    }

    fn hand_jobs(&self) -> usize {
        self.jobs
            .jobs
            .values()
            .filter(|job| job.kind == slice_dispatch::KIND)
            .count()
    }
}

fn published() -> String {
    format!("agent exit 0 in 9s\nrunner publish: pushed refs/heads/vivi/need/{NEED} {HAND} {TIP}")
}

const RED: &str = "--- stderr tail\ntest result: FAILED. 1 failed\nagent exit 101 in 40s";

#[test]
fn a_merged_receipt_closes_the_slice_merged() -> R {
    let mut rig = rig()?;
    let slice = rig.slice(JobStatus::Finished, &published(), 1)?;
    rig.apply()?;
    assert!(rig.mail.calls.contains(&Call::CloseTask {
        handle: slice.task.clone(),
        verdict: String::from("merged"),
    }));
    let row = rig.row(&slice)?;
    assert_eq!(row.state, StepState::Done);
    assert_eq!(row.verdict.as_deref(), Some("merged"));
    assert!(rig.merge_job(1).applied_at.is_some());
    Ok(())
}

#[test]
fn a_red_receipt_posts_the_reason_and_queues_hand_attempt_two() -> R {
    let mut rig = rig()?;
    let slice = rig.slice(JobStatus::Finished, RED, 1)?;
    rig.apply()?;
    assert_eq!(rig.closes(), 0);
    let replies = rig.replies();
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert!(
        replies[0].contains("format or task rung failed"),
        "{replies:?}"
    );
    assert!(replies[0].contains("test result: FAILED"), "{replies:?}");
    assert!(
        rig.jobs
            .jobs
            .contains_key(&format!("slice-{}-a2", slice.task))
    );
    assert_eq!(rig.row(&slice)?.state, StepState::Running);
    assert!(rig.merge_job(1).applied_at.is_some());
    Ok(())
}

#[test]
fn a_red_receipt_on_attempt_two_exhausts_the_slice() -> R {
    let mut rig = rig()?;
    let slice = rig.slice(JobStatus::Finished, RED, 2)?;
    let hands = rig.hand_jobs();
    rig.apply()?;
    assert_eq!(rig.closes(), 0);
    let row = rig.row(&slice)?;
    assert_eq!(row.state, StepState::Failed);
    assert_eq!(row.verdict.as_deref(), Some("failed"));
    assert_eq!(rig.hand_jobs(), hands, "no third attempt");
    assert!(rig.merge_job(2).applied_at.is_some());
    Ok(())
}

#[test]
fn a_conflict_lists_its_paths_in_the_reply() -> R {
    let mut rig = rig()?;
    rig.slice(
        JobStatus::Finished,
        "CONFLICT (content): Merge conflict in src/b.rs\nCONFLICT (content): Merge conflict in src/a.rs\nagent exit 1 in 2s",
        1,
    )?;
    rig.apply()?;
    let replies = rig.replies();
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert!(
        replies[0].contains("conflict: src/a.rs, src/b.rs"),
        "{replies:?}"
    );
    assert_eq!(rig.closes(), 0);
    Ok(())
}

#[test]
fn a_job_of_a_closed_slice_is_only_marked_applied() -> R {
    let mut rig = rig()?;
    let slice = rig.slice(JobStatus::Finished, &published(), 1)?;
    rig.store
        .set_step_state(&slice.task, StepState::Done, Some("merged"))?;
    let hands = rig.hand_jobs();
    rig.apply()?;
    assert!(
        rig.mail
            .calls
            .iter()
            .all(|call| !matches!(call, Call::CloseTask { .. }))
    );
    assert!(rig.replies().is_empty());
    assert_eq!(rig.hand_jobs(), hands);
    assert!(rig.merge_job(1).applied_at.is_some());
    Ok(())
}

#[test]
fn a_job_of_a_superseded_run_is_only_marked_applied() -> R {
    let mut rig = rig()?;
    rig.slice(JobStatus::Finished, &published(), 1)?;
    rig.store.set_run_state(NEED, 1, "superseded")?;
    rig.apply()?;
    assert_eq!(rig.closes(), 0);
    assert!(rig.merge_job(1).applied_at.is_some());
    Ok(())
}

#[test]
fn an_applied_job_is_not_applied_twice() -> R {
    let mut rig = rig()?;
    rig.slice(JobStatus::Finished, RED, 1)?;
    rig.apply()?;
    rig.apply()?;
    assert_eq!(rig.replies().len(), 1);
    Ok(())
}

#[test]
fn an_unclear_receipt_is_closed_when_git_shows_the_hand_commit_landed() -> R {
    let mut rig = rig()?;
    let slice = rig.slice(JobStatus::Lost, "", 1)?;
    rig.host.reply(
        "ls-remote",
        0,
        &format!("{TIP}\trefs/heads/vivi/need/{NEED}\n"),
    );
    rig.host.reply("merge-base --is-ancestor", 0, "");
    rig.apply()?;
    assert_eq!(rig.row(&slice)?.state, StepState::Done);
    assert_eq!(rig.closes(), 1);
    assert!(rig.merge_job(1).applied_at.is_some());
    Ok(())
}

#[test]
fn an_unclear_receipt_with_an_unmoved_branch_is_a_failed_attempt() -> R {
    let mut rig = rig()?;
    let slice = rig.slice(JobStatus::Lost, "", 1)?;
    rig.host.reply(
        "ls-remote",
        0,
        &format!("{BEFORE}\trefs/heads/vivi/need/{NEED}\n"),
    );
    rig.apply()?;
    assert_eq!(rig.closes(), 0);
    let replies = rig.replies();
    assert!(
        replies[0].contains("left no change on the need branch"),
        "{replies:?}"
    );
    assert!(
        rig.jobs
            .jobs
            .contains_key(&format!("slice-{}-a2", slice.task))
    );
    Ok(())
}

#[test]
fn an_unclear_receipt_git_cannot_settle_holds_the_slice_for_the_operator() -> R {
    let mut rig = rig()?;
    let slice = rig.slice(JobStatus::Lost, "", 1)?;
    rig.host.reply("ls-remote", 128, "network down");
    rig.apply()?;
    let row = rig.row(&slice)?;
    assert_eq!(row.state, StepState::Waiting);
    assert_eq!(row.verdict.as_deref(), Some(OPERATOR_HOLD));
    assert_eq!(rig.closes(), 0);
    let raised = fs::read_to_string(rig.ctx.home.join("exceptions.txt")).unwrap_or_default();
    assert!(raised.contains("flow-side-effect-unclear"), "{raised}");
    assert!(rig.merge_job(1).applied_at.is_some());
    Ok(())
}

const NO_TSC: &str = "agent exit 127 in 1s\n\n--- stderr tail\nbash: tsc: command not found\n";

#[test]
fn an_environment_failure_alarms_once_and_holds_the_slice() -> R {
    let mut rig = rig()?;
    let slice = rig.slice(JobStatus::Finished, NO_TSC, 1)?;
    let hands = rig.hand_jobs();
    rig.apply()?;
    rig.apply()?;
    let row = rig.row(&slice)?;
    assert_eq!(row.state, StepState::Waiting);
    assert_eq!(row.verdict.as_deref(), Some(OPERATOR_HOLD));
    assert_eq!(rig.closes(), 0);
    assert_eq!(rig.hand_jobs(), hands, "no hand attempt is queued");
    let raised = fs::read_to_string(rig.ctx.home.join("exceptions.txt")).unwrap_or_default();
    assert_eq!(raised.matches("flow-env-failure").count(), 1, "{raised}");
    assert!(raised.contains("tsc"), "{raised}");
    assert!(raised.contains("command not found"), "{raised}");
    let replies = rig.replies();
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert!(replies[0].contains("environment: "), "{replies:?}");
    assert!(rig.merge_job(1).applied_at.is_some());
    Ok(())
}

#[test]
fn an_environment_failure_on_the_last_attempt_does_not_exhaust_the_slice() -> R {
    let mut rig = rig()?;
    let slice = rig.slice(JobStatus::Finished, NO_TSC, 2)?;
    rig.apply()?;
    let row = rig.row(&slice)?;
    assert_eq!(row.state, StepState::Waiting);
    assert_eq!(row.verdict.as_deref(), Some(OPERATOR_HOLD));
    Ok(())
}

#[test]
fn a_retry_that_fails_once_posts_the_reply_once_and_queues_attempt_two_once() -> R {
    let mut rig = rig()?;
    let slice = rig.slice(JobStatus::Finished, RED, 1)?;
    let with_hand = (*rig.ctx.routing).clone();
    let mut without_hand = with_hand.clone();
    without_hand.roles.retain(|role| role.name != "hand");
    rig.ctx = rig.ctx.with_routing(without_hand);
    rig.apply()?;
    assert!(
        rig.merge_job(1).applied_at.is_none(),
        "a failed retry leaves the job unapplied"
    );
    assert_eq!(rig.replies().len(), 1);
    rig.ctx = rig.ctx.with_routing(with_hand);
    rig.apply()?;
    assert!(rig.merge_job(1).applied_at.is_some());
    assert_eq!(rig.replies().len(), 1, "{:?}", rig.replies());
    let queued = rig
        .jobs
        .jobs
        .keys()
        .filter(|id| id.as_str() == format!("slice-{}-a2", slice.task))
        .count();
    assert_eq!(queued, 1);
    Ok(())
}

#[test]
fn a_payload_attempt_that_is_not_a_number_is_a_parse_error_and_posts_nothing() -> R {
    let mut rig = rig()?;
    rig.slice(JobStatus::Finished, RED, 1)?;
    let hands = rig.hand_jobs();
    if let Some(job) = rig.jobs.jobs.get_mut("hand-merge-1") {
        job.payload["attempt"] = serde_json::json!("x");
    }
    rig.apply()?;
    assert!(rig.replies().is_empty());
    assert_eq!(rig.hand_jobs(), hands);
    assert!(rig.merge_job(1).applied_at.is_none());
    let raised = fs::read_to_string(rig.ctx.home.join("exceptions.txt")).unwrap_or_default();
    assert!(raised.contains("hand-merge-1"), "{raised}");
    assert!(raised.contains("attempt"), "{raised}");
    Ok(())
}
