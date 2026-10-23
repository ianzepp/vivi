//! Tests for the flow tick: withdrawal, role collection, corrections, slice
//! results, blocked-scope round trips, side-effect verification, finishing
//! runs, alarms and the per-run error boundary.

use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tempfile::{TempDir, tempdir};

use super::{
    FINISHING_STUCK, FOLLOW_UP, FOLLOW_UP_MARKER, KIND_CHANGED, OPERATOR_HOLD, RUN_ERROR,
    SCOPE_REWORK, SIDE_EFFECT_UNCLEAR, STEP_OVERDUE, VALIDATED, tick_with,
};
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::flow::advance::start::{SENTINEL_NODE, start};
use crate::pump::flow::def::FlowKind;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{RunRow, StepJobRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::{MailWriter, TaskStatus};
use crate::pump::flow::need_branch;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::registry;
use crate::pump::flow::scope::ScopeState;
use crate::pump::flow::slice_dispatch::{self, DEVIATIONS_MARKER};
use crate::pump::flow::validate::validate;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{self, JobOutcome, JobSpec, JobStatus, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

const NEED: &str = "n1";
const BASE: &str = "2222222222222222222222222222222222222222";
const BEFORE: &str = "1111111111111111111111111111111111111111";
const TIP: &str = "2222222222222222222222222222222222222222";
const WANT: &str = "3333333333333333333333333333333333333333";

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

/// A live need-branch row on the slices' target, so hands have a start point.
fn seed_need_branch(store: &PumpStore) -> R {
    store.upsert_need_branch(&NeedBranchRow {
        need: NEED.into(),
        branch: need_branch::branch(NEED),
        base: Some(BASE.into()),
        status: need_branch::LIVE.into(),
        run: 1,
        target: "default".into(),
        priors: Vec::new(),
    })
}

fn rig() -> Result<Rig, VivariumError> {
    rig_with("kind: build")
}

fn rig_with(record: &str) -> Result<Rig, VivariumError> {
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
    let ctx = ctx.with_routing(routing);
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    seed_need_branch(&store)?;
    let mut mail = FakeMail::new("mind");
    MailWriter::open_record(&mut mail, NEED, record)?;
    Ok(Rig {
        _dir: dir,
        ctx,
        host,
        store,
        mail,
        jobs: Jobs::default(),
    })
}

fn receipt(push: &str, exit: i32, message: &str) -> String {
    format!(
        "pool result j1: finished\npool p  slot s1  model m\nfrom main  default\n\
         push: {push}\nagent exit {exit} in 12s, job 20s, started from 1234567890\n\n\
         --- receipt\n{message}\n\n--- stderr tail\n\n\n--- runner log tail\nlog line"
    )
}

fn epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

const PLANNED: &str = "+++\nanswer = \"planned\"\n+++\nthe plan";
const BLOCKED: &str = "+++\nanswer = \"blocked-scope\"\nscope_paths = [\"src/extra.rs\"]\n\
                       scope_reason = 'needs it'\n+++\nnothing changed";
const BUILT_FULL: &str = "+++\nanswer = \"built\"\ndeviations = ['changed a signature']\n\n\
                          [[follow_up]]\npaths = [\"src/b.rs\"]\nneed = 'do the other thing'\n+++\ndone";
const BUILT: &str = "+++\nanswer = \"built\"\n+++\ndone";

impl Rig {
    fn start(&mut self, entry: &str) -> Result<RunRow, VivariumError> {
        start(&self.ctx, &self.store, &mut self.mail, NEED, Some(entry))
    }

    fn step(&self, node: &str) -> Result<StepRow, VivariumError> {
        self.store
            .steps_of(NEED, 1)?
            .into_iter()
            .rev()
            .find(|row| row.node == node)
            .ok_or_else(|| VivariumError::Message(format!("no step {node}")))
    }

    fn tick(&mut self) -> R {
        tick_with(
            &self.ctx,
            &mut self.jobs,
            &self.store,
            &mut self.mail,
            &Registry::new(),
        )
    }

    fn exceptions(&self) -> String {
        fs::read_to_string(self.ctx.home.join("exceptions.txt")).unwrap_or_default()
    }

    fn count(&self, kind: &str) -> usize {
        self.exceptions().matches(kind).count()
    }

    /// A finished job of `node`'s step whose final message is `message`.
    fn role_job(&mut self, node: &str, id: &str, message: &str) -> R {
        let step = self.step(node)?;
        let mut spec = JobSpec::new(id, "planner", "pump-planner", "s", "b");
        spec.task = Some(step.task.clone());
        jobs::enqueue(&mut self.jobs, spec)?;
        self.store.insert_step_job(
            NEED,
            &StepJobRow {
                job: id.into(),
                run: 1,
                node: node.into(),
                task: step.task.clone(),
                attempt: 1,
            },
        )?;
        self.store
            .set_step_state(&step.task, StepState::Running, None)?;
        self.finish(id, &receipt("", 0, message));
        Ok(())
    }

    fn finish(&mut self, id: &str, receipt: &str) {
        let job = self.jobs.jobs.get_mut(id).unwrap();
        job.status = JobStatus::Finished;
        job.outcome = Some(JobOutcome {
            state: "finished".into(),
            detail: String::new(),
            receipt: receipt.into(),
            at: 0,
        });
    }

    /// A pending slice with `scope`, on the started run.
    fn slice(&mut self, id: &str, scope: &[&str]) -> Result<StepRow, VivariumError> {
        let task = self.mail.send_step(
            "mind",
            "mind",
            &format!("slice {id}"),
            &format!(
                "pump-step: {NEED} r1 slice:{id}\ntarget: default\n\ntitle {id}\n\nbrief {id}"
            ),
            &[],
        )?;
        let row = StepRow {
            run: 1,
            node: format!("slice:{id}"),
            task: task.clone(),
            state: StepState::Pending,
            verdict: None,
        };
        self.store.insert_step(NEED, &row)?;
        let scope: Vec<String> = scope.iter().map(|path| (*path).to_string()).collect();
        self.store.insert_slice_scope(&task, 1, &scope)?;
        Ok(row)
    }

    fn dispatch(&mut self, slice: &StepRow) -> Result<String, VivariumError> {
        slice_dispatch::dispatch(
            &self.ctx,
            &mut self.jobs,
            &self.store,
            &self.mail,
            NEED,
            slice,
            1,
        )
    }

    /// A merge-main step of the build flow in the running state.
    fn merge_step(&mut self, id: &str, status: JobStatus) -> Result<StepRow, VivariumError> {
        let task = self.mail.send_step(
            "mind",
            "mind",
            "merge",
            &format!("pump-step: {NEED} r1 merge"),
            &[],
        )?;
        let row = StepRow {
            run: 1,
            node: "merge".into(),
            task: task.clone(),
            state: StepState::Running,
            verdict: None,
        };
        self.store.insert_step(NEED, &row)?;
        let mut spec = JobSpec::new(id, "merge", "merge", "s", "b");
        spec.task = Some(task.clone());
        spec.target = Some("default".into());
        spec.from_ref = Some(BEFORE.into());
        spec.publish_ref = Some("refs/heads/main".into());
        spec.publish_contains = Some(WANT.into());
        jobs::enqueue(&mut self.jobs, spec)?;
        self.store.insert_step_job(
            NEED,
            &StepJobRow {
                job: id.into(),
                run: 1,
                node: "merge".into(),
                task,
                attempt: 1,
            },
        )?;
        self.jobs.jobs.get_mut(id).unwrap().status = status;
        Ok(row)
    }

    fn run_state(&self) -> Result<String, VivariumError> {
        Ok(self.store.runs_of(NEED)?[0].state.clone())
    }
}

// ---- withdrawal and role collection -------------------------------------------

#[test]
fn a_closed_need_withdraws_its_run() -> R {
    let mut rig = rig()?;
    rig.start("plan")?;
    rig.mail.close_need_by(NEED, "operator");
    rig.tick()?;
    assert_eq!(rig.run_state()?, "withdrawn");
    assert_eq!(rig.mail.record_outcome(NEED), Some("withdrawn"));
    Ok(())
}

#[test]
fn a_finished_role_job_with_a_valid_envelope_advances() -> R {
    let mut rig = rig()?;
    rig.start("plan")?;
    rig.role_job("plan", "plan-j1", PLANNED)?;
    rig.tick()?;
    let plan = rig.step("plan")?;
    assert_eq!(plan.state, StepState::Done);
    assert_eq!(plan.verdict.as_deref(), Some("planned"));
    assert_eq!(rig.step("admit1")?.state, StepState::Pending);
    assert!(rig.jobs.jobs["plan-j1"].applied_at.is_some());
    Ok(())
}

#[test]
fn a_broken_envelope_gets_one_correction_and_the_corrected_output_advances() -> R {
    let mut rig = rig()?;
    rig.start("plan")?;
    rig.role_job("plan", "plan-j1", "no envelope at all")?;
    rig.tick()?;

    assert_eq!(rig.step("plan")?.state, StepState::Running);
    let fix = rig.store.step_job("plan-j1-c1")?.unwrap();
    assert_eq!(fix.attempt, 1, "a correction uses no attempt");
    assert_eq!(rig.jobs.jobs.len(), 2);
    assert_eq!(rig.jobs.jobs["plan-j1-c1"].kind, "envelope-fix");

    rig.tick()?;
    assert_eq!(
        rig.jobs.jobs.len(),
        2,
        "the pending correction is not redone"
    );
    rig.finish("plan-j1-c1", &receipt("", 0, PLANNED));
    rig.tick()?;
    let plan = rig.step("plan")?;
    assert_eq!(
        (plan.state, plan.verdict.as_deref()),
        (StepState::Done, Some("planned"))
    );
    Ok(())
}

// ---- slices ---------------------------------------------------------------------

#[test]
fn a_built_hand_posts_deviations_and_follow_ups_and_stays_open() -> R {
    let mut rig = rig()?;
    rig.start("plan")?;
    let slice = rig.slice("a", &["src/a.rs"])?;
    let id = rig.dispatch(&slice)?;
    rig.finish(
        &id,
        &receipt("pushed vivi/unit/x-a1 abcdef0123", 0, BUILT_FULL),
    );

    rig.tick()?;

    let replies = &rig.mail.tasks[&slice.task].replies;
    assert!(
        replies
            .iter()
            .any(|r| r.starts_with(&format!("{DEVIATIONS_MARKER} t="))
                && r.contains("changed a signature"))
    );
    assert!(replies.iter().any(|r| r.starts_with(FOLLOW_UP_MARKER)
        && r.contains("[[follow_up]]")
        && r.contains("src/b.rs")));
    assert_eq!(rig.count(FOLLOW_UP), 1);
    assert_eq!(rig.mail.tasks[&slice.task].state.status, TaskStatus::Open);
    assert!(rig.jobs.jobs[&id].applied_at.is_some());
    rig.tick()?;
    assert_eq!(rig.count(FOLLOW_UP), 1, "applied once");
    Ok(())
}

#[test]
fn a_hand_that_changed_nothing_closes_its_slice_no_defect() -> R {
    let mut rig = rig()?;
    rig.start("plan")?;
    let slice = rig.slice("a", &["src/a.rs"])?;
    let id = rig.dispatch(&slice)?;
    rig.finish(&id, &receipt("no_changes", 0, BUILT));
    rig.tick()?;
    let state = &rig.mail.tasks[&slice.task].state;
    assert_eq!(state.status, TaskStatus::Done);
    assert_eq!(state.verdict.as_deref(), Some("no-defect"));
    assert_eq!(rig.step(&slice.node)?.state, StepState::Done);
    Ok(())
}

#[test]
fn a_failed_hand_attempt_retries_the_same_task() -> R {
    let mut rig = rig()?;
    rig.start("plan")?;
    let slice = rig.slice("a", &["src/a.rs"])?;
    let id = rig.dispatch(&slice)?;
    rig.finish(&id, &receipt("", 1, "crashed"));
    rig.tick()?;
    let second = format!("slice-{}-a2", slice.task);
    assert_eq!(
        rig.jobs.jobs[&second].task.as_deref(),
        Some(slice.task.as_str())
    );
    assert_eq!(rig.store.step_job(&second)?.unwrap().attempt, 2);
    Ok(())
}

fn blocked_receipt() -> String {
    receipt("no_changes", 0, BLOCKED)
}

#[test]
fn blocked_scope_with_no_holder_widens_and_redispatches_the_same_attempt() -> R {
    let mut rig = rig()?;
    rig.start("plan")?;
    let slice = rig.slice("a", &["src/a.rs"])?;
    let id = rig.dispatch(&slice)?;
    rig.finish(&id, &blocked_receipt());

    rig.tick()?;

    let again = format!("slice-{}-a1-s1", slice.task);
    assert!(rig.jobs.jobs[&again].body.contains("- src/extra.rs"));
    assert_eq!(rig.store.step_job(&again)?.unwrap().attempt, 1);
    assert_eq!(rig.store.scope_state(&slice.task)?, Some(ScopeState::Clear));
    Ok(())
}

#[test]
fn blocked_scope_held_by_a_slice_waits_then_redispatches_after_it_merges() -> R {
    let mut rig = rig()?;
    rig.start("plan")?;
    let a = rig.slice("a", &["src/a.rs"])?;
    let b = rig.slice("b", &["src/extra.rs"])?;
    let id = rig.dispatch(&a)?;
    rig.finish(&id, &blocked_receipt());

    rig.tick()?;

    assert!(rig.mail.tasks[&a.task].depends_on.contains(&b.task));
    assert_eq!(rig.store.scope_state(&a.task)?, Some(ScopeState::Wait));
    assert_eq!(rig.jobs.jobs.len(), 1, "waiting queues nothing");

    rig.tick()?;
    assert_eq!(rig.jobs.jobs.len(), 1, "the holder is still open");

    rig.mail.close_task(&b.task, "merged", None, None)?;
    rig.tick()?;
    let again = format!("slice-{}-a1-s1", rig.step(&a.node)?.task);
    assert!(
        rig.jobs.jobs.contains_key(&again),
        "{:?}",
        rig.jobs.jobs.keys()
    );
    assert!(
        rig.jobs.jobs[&again].body.contains("- src/extra.rs"),
        "the formerly held path is granted"
    );
    assert!(
        !rig.jobs.jobs[&id].body.contains("- src/extra.rs"),
        "the first brief did not have it"
    );
    assert_eq!(rig.store.scope_state(&a.task)?, Some(ScopeState::Clear));

    // The request that ended in the wait still counts: the next block after
    // it is the second, so it goes to scope rework and nothing is re-queued.
    rig.finish(&again, &blocked_receipt());
    rig.tick()?;
    assert_eq!(rig.store.scope_state(&a.task)?, Some(ScopeState::Rework));
    assert_eq!(rig.count(SCOPE_REWORK), 1);
    assert_eq!(rig.jobs.jobs.len(), 2, "no third job");
    Ok(())
}

#[test]
fn a_second_blocked_request_goes_to_scope_rework() -> R {
    let mut rig = rig()?;
    rig.start("plan")?;
    let a = rig.slice("a", &["src/a.rs"])?;
    let id = rig.dispatch(&a)?;
    rig.finish(&id, &blocked_receipt());
    rig.tick()?;
    let again = format!("slice-{}-a1-s1", a.task);
    rig.finish(&again, &blocked_receipt());

    rig.tick()?;

    assert_eq!(rig.store.scope_state(&a.task)?, Some(ScopeState::Rework));
    assert_eq!(rig.count(SCOPE_REWORK), 1);
    assert_eq!(rig.jobs.jobs.len(), 2, "no third job");
    Ok(())
}

// ---- incident rules -------------------------------------------------------------

#[test]
fn a_lost_merge_whose_ref_shows_the_merge_is_recorded_merged_not_failed() -> R {
    let mut rig = rig()?;
    rig.start("plan")?;
    let step = rig.merge_step("merge-j1", JobStatus::Lost)?;
    rig.host
        .reply("ls-remote", 0, &format!("{TIP}\trefs/heads/main\n"));
    rig.host.reply("merge-base --is-ancestor", 0, "");

    rig.tick()?;

    let state = &rig.mail.tasks[&step.task].state;
    assert_eq!(state.verdict.as_deref(), Some("merged"));
    assert_eq!(rig.mail.tasks[&step.task].commit.as_deref(), Some(TIP));
    assert_eq!(rig.run_state()?, "done");
    Ok(())
}

#[test]
fn a_lost_merge_whose_ref_did_not_move_answers_failed() -> R {
    let mut rig = rig()?;
    rig.start("plan")?;
    let step = rig.merge_step("merge-j1", JobStatus::Lost)?;
    rig.host
        .reply("ls-remote", 0, &format!("{BEFORE}\trefs/heads/main\n"));

    rig.tick()?;

    let state = &rig.mail.tasks[&step.task].state;
    assert_eq!(state.verdict.as_deref(), Some("failed"));
    assert_eq!(rig.step("cto")?.state, StepState::Pending);
    Ok(())
}

#[test]
fn an_unclear_merge_alarms_and_holds_for_the_operator() -> R {
    let mut rig = rig()?;
    rig.start("plan")?;
    let step = rig.merge_step("merge-j1", JobStatus::Lost)?;
    rig.host.reply("ls-remote", 1, "network down");

    rig.tick()?;
    rig.tick()?;

    let held = rig.step("merge")?;
    assert_eq!(held.state, StepState::Waiting);
    assert_eq!(held.verdict.as_deref(), Some(OPERATOR_HOLD));
    assert_eq!(rig.mail.tasks[&step.task].state.status, TaskStatus::Open);
    assert_eq!(rig.count(SIDE_EFFECT_UNCLEAR), 1);
    let text = rig.exceptions();
    assert!(
        text.contains("merge-j1") && text.contains("refs/heads/main"),
        "{text}"
    );
    Ok(())
}

#[test]
fn an_overdue_job_alarms_once_naming_its_evidence() -> R {
    let mut rig = rig()?;
    rig.start("plan")?;
    rig.role_job("plan", "plan-j1", PLANNED)?;
    let job = rig.jobs.jobs.get_mut("plan-j1").unwrap();
    job.status = JobStatus::Dispatched;
    job.outcome = None;
    job.dispatched_at = Some(1_000);
    job.budget = 100;
    job.pool = Some("pool-x".into());

    rig.host.set_now(1_000 + 100 + 600);
    rig.tick()?;
    assert_eq!(rig.count(STEP_OVERDUE), 0, "within the grace");

    rig.host.set_now(1_000 + 100 + 601);
    rig.tick()?;
    rig.tick()?;
    assert_eq!(rig.count(STEP_OVERDUE), 1);
    let text = rig.exceptions();
    assert!(
        text.contains("plan-j1") && text.contains("pool-x") && text.contains("plan"),
        "{text}"
    );
    assert_eq!(
        rig.step("plan")?.state,
        StepState::Running,
        "nothing changed"
    );
    Ok(())
}

#[test]
fn a_kind_change_alarms_once_and_changes_nothing() -> R {
    let mut rig = rig_with("a record without a kind line")?;
    rig.start("plan")?;
    rig.mail.set_marks(NEED, &["research"]);
    rig.tick()?;
    rig.tick()?;
    assert_eq!(rig.count(KIND_CHANGED), 1);
    assert_eq!(rig.run_state()?, "live");
    Ok(())
}

#[test]
fn the_landing_tick_of_a_record_kind_run_raises_no_kind_alarm() -> R {
    let mut rig = rig_with("kind: research")?;
    rig.start("research")?;
    rig.role_job(
        "research",
        "research-j1",
        "+++\nanswer = \"READY\"\n+++\nanalysis",
    )?;
    rig.tick()?;
    assert_eq!(rig.run_state()?, "done");
    assert_eq!(rig.count(KIND_CHANGED), 0);
    Ok(())
}

#[test]
fn a_real_kind_change_on_a_live_record_kind_run_still_alarms_once() -> R {
    let mut rig = rig_with("a record without a kind line")?;
    rig.mail.set_marks(NEED, &["research"]);
    rig.start("research")?;
    assert_eq!(rig.store.runs_of(NEED)?[0].kind, "research");
    rig.mail.set_marks(NEED, &["repair"]);
    rig.tick()?;
    rig.tick()?;
    assert_eq!(rig.count(KIND_CHANGED), 1);
    assert_eq!(rig.run_state()?, "live");
    Ok(())
}

#[test]
fn a_record_closed_without_landing_raises_no_kind_alarm() -> R {
    let mut rig = rig_with("kind: research")?;
    rig.start("research")?;
    rig.mail.close_need_by(NEED, "operator");
    rig.tick()?;
    assert_eq!(rig.count(KIND_CHANGED), 0);
    Ok(())
}

#[test]
fn an_error_in_one_run_does_not_stop_another_run() -> R {
    let mut rig = rig()?;
    rig.start("plan")?;
    rig.role_job("plan", "plan-j1", PLANNED)?;
    rig.store.insert_run(&RunRow {
        need: "n0".into(),
        run: 1,
        flow: "bogus".into(),
        flow_version: 1,
        kind: "bogus".into(),
        entry: "plan".into(),
        state: "live".into(),
    })?;

    rig.tick()?;

    assert_eq!(rig.count(RUN_ERROR), 1);
    assert_eq!(rig.step("plan")?.verdict.as_deref(), Some("planned"));
    Ok(())
}

// ---- finishing runs -------------------------------------------------------------

#[test]
fn a_finishing_run_is_completed_without_a_withdrawal() -> R {
    let mut rig = rig()?;
    rig.start("plan")?;
    rig.store.set_run_state(NEED, 1, "finishing")?;

    rig.tick()?;

    assert_eq!(rig.run_state()?, "done");
    assert_eq!(rig.mail.record_outcome(NEED), Some("landed"));
    assert_eq!(
        rig.mail.need_close_note(NEED),
        Some("landed by the pump flow")
    );
    rig.tick()?;
    assert_eq!(rig.run_state()?, "done");
    Ok(())
}

#[test]
fn a_run_that_keeps_failing_to_finish_raises_the_stuck_alarm_once() -> R {
    let mut rig = rig()?;
    rig.store.insert_run(&RunRow {
        need: NEED.into(),
        run: 1,
        flow: "build".into(),
        flow_version: 1,
        kind: "build".into(),
        entry: "plan".into(),
        state: "live".into(),
    })?;
    // No sentinel row: `resume` cannot complete.
    rig.store.set_run_state(NEED, 1, "finishing")?;

    rig.host.set_now(epoch());
    rig.tick()?;
    assert_eq!(rig.count(FINISHING_STUCK), 0, "not stuck yet");

    rig.host.set_now(epoch() + 700);
    rig.tick()?;
    rig.tick()?;
    assert_eq!(rig.count(FINISHING_STUCK), 1);
    assert!(rig.exceptions().contains("n1"));
    assert_eq!(rig.run_state()?, "finishing");
    assert!(
        rig.store
            .steps_of(NEED, 1)?
            .iter()
            .all(|s| s.node != SENTINEL_NODE)
    );
    Ok(())
}

// ---- start answers and startup validation ---------------------------------------

struct AnswersAtStart;

impl StepHandler for AnswersAtStart {
    fn start(&self, _cx: &StartCx<'_>, _step: &StepRow) -> Result<StartAction, VivariumError> {
        Ok(StartAction::Answer {
            word: "planned".into(),
            payload: serde_json::json!({"reason": "r"}),
        })
    }
}

#[test]
fn a_start_answer_advances_the_step_and_records_its_payload() -> R {
    let mut rig = rig()?;
    rig.start("plan")?;
    let mut registry = Registry::new();
    registry.node_handler(FlowKind::Build, "plan", Box::new(AnswersAtStart));

    tick_with(
        &rig.ctx,
        &mut rig.jobs,
        &rig.store,
        &mut rig.mail,
        &registry,
    )?;

    let plan = rig.step("plan")?;
    assert_eq!(plan.state, StepState::Done);
    assert_eq!(plan.verdict.as_deref(), Some("planned"));
    let stored = rig.store.step_payload(&plan.task)?.expect("payload row");
    assert_eq!(stored.payload, serde_json::json!({"reason": "r"}));
    Ok(())
}

#[test]
fn every_shipped_flow_is_validated_at_startup() -> R {
    let kinds = [
        FlowKind::Build,
        FlowKind::Research,
        FlowKind::Release,
        FlowKind::Repair,
    ];
    for kind in kinds {
        assert!(registry::shipped(kind).is_some(), "{} ships", kind.name());
        assert!(VALIDATED.contains(&kind), "{} is validated", kind.name());
        let def = load(kind)?;
        validate(&def).map_err(|problems| VivariumError::Parse(problems.join("; ")))?;
    }
    assert_eq!(VALIDATED.len(), kinds.len());
    Ok(())
}
