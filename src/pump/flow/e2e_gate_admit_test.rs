//! End to end gate: the admit, plan-revision and slice edges.
//!
//! Region: `admit1`, `plan2`, `admit2` and `slice`. Edges covered: admit1
//! `revise`, `escalate`, `failed`; plan2 `planned`, `delivered`, `operator`,
//! `hold`, `design`, `failed`; admit2 `admitted`, `revise`, `escalate`,
//! `failed`; slice `escalate`, `failed`, and `sliced` into a refused fanout
//! that holds the CTO.
//!
//! Each test runs the build flow from `plan` on a real mailspace through the
//! real tick, finishes every job with a scripted receipt as the pool would,
//! and asserts the next open step. A role that fails does so the way the pool
//! makes it fail: two lost jobs, so the one retry is spent. One test answers
//! the held CTO as the operator would, through the mailspace: the branch-less
//! `rework` is refused once, and a `plan` ruling restarts the need.

use super::e2e_rig::{Rig, agent_receipt, rig};
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::flow::answer::ANSWER_REFUSED;
use crate::pump::flow::fanout::SLICES_INVALID;
use crate::pump::flow::handlers::shipped;
use crate::pump::flow::index::StepState;
use crate::pump::flow::mail::{MailReader, NeedStatus, TaskStatus};
use crate::pump::flow::slice_dispatch;
use crate::pump::flow::step_error::{STEP_ERROR_HELD, STEP_ERROR_LIMIT};
use crate::pump::flow::tick::{OPERATOR_HOLD, RUN_ERROR, WORD_REJECTED};
use crate::pump::jobs::JobStatus;
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

/// The alarm a parked run raises, once.
const PARKED_ALARM: &str = "flow-need-parked";

/// A build run started at `plan`, with an admitter and a slicer routed and the
/// need thread readable.
fn build() -> Result<Rig, VivariumError> {
    let mut rig = rig(
        PumpConfig::default(),
        "+++\nkind = \"build\"\n+++\n",
        "plan",
    )?;
    let mut routing = (*rig.ctx.routing).clone();
    for (name, kind, read_only) in [
        ("admitter", "admitter", Some(true)),
        ("slicer", "slicer", Some(true)),
    ] {
        routing.roles.push(RoutedRole {
            name: name.into(),
            kind: Some(kind.into()),
            status: String::from("active"),
            pool: Some(String::from("test-planner-pool")),
            read_only,
            model: None,
            models: Vec::new(),
        });
    }
    rig.ctx = rig.ctx.with_routing(routing);
    rig.registry = shipped(&rig.ctx);
    let shown = serde_json::json!([
        {"handle": rig.need, "message_id": "m1", "from": "operator@local",
         "subject": "Need", "body": "Build the thing"}
    ]);
    rig.project.host.reply("need show", 0, &shown.to_string());
    Ok(rig)
}

/// Tick until `node` has an open job, then return its id.
fn open(rig: &mut Rig, node: &str) -> Result<String, VivariumError> {
    rig.tick_until(&format!("a {node} job"), |rig| rig.open_job(node).is_some())?;
    rig.open_job(node)
        .map(|job| job.id)
        .ok_or_else(|| VivariumError::Message(format!("no open {node} job")))
}

/// Finish job `id` with `receipt`, and record the settled job on its step task
/// as the pool does: later steps read the earlier plan and reasons from there.
fn finish(rig: &mut Rig, id: &str, receipt: &str) -> R {
    rig.finish(id, JobStatus::Finished, receipt)?;
    let row = rig
        .store
        .step_job(id)?
        .ok_or_else(|| VivariumError::Message(format!("job {id} has no step row")))?;
    rig.port.mailspace.record_pool_note(
        &row.task,
        "pool_collected",
        &format!("{{\"job\":\"{id}\"}}"),
        receipt,
    )
}

/// Tick until `node` has an open job, then finish it with an envelope whose
/// answer is `answer` and whose extra lines are `extra`.
fn answer(rig: &mut Rig, node: &str, answer: &str, extra: &str) -> R {
    let id = open(rig, node)?;
    let envelope = format!("+++\nanswer = \"{answer}\"\n{extra}+++\n{answer}");
    finish(rig, &id, &agent_receipt(0, &envelope))
}

/// Lose `node`'s job twice: the retry is spent and the step answers `failed`.
fn lose(rig: &mut Rig, node: &str) -> R {
    for _ in 0..2 {
        let id = open(rig, node)?;
        rig.finish(&id, JobStatus::Lost, "")?;
    }
    Ok(())
}

fn verdict(rig: &Rig, node: &str) -> Result<Option<String>, VivariumError> {
    Ok(rig.row(node)?.and_then(|row| row.verdict))
}

/// The planner asks for a change: the answers other than `planned` carry a
/// reason.
const REASON: &str = "reason = 'because'\n";
/// The admitter asks for a change.
const CHANGES: &str = "required_changes = ['tighten the scope']\n";

/// Plan, then an admitter that asks to revise: the run is at `plan2`.
fn to_plan2(rig: &mut Rig) -> R {
    answer(rig, "plan", "planned", "")?;
    answer(rig, "admit1", "revise", CHANGES)?;
    open(rig, "plan2").map(drop)
}

/// Through the revision to the `admit2` job.
fn to_admit2(rig: &mut Rig) -> R {
    to_plan2(rig)?;
    answer(rig, "plan2", "planned", "")?;
    open(rig, "admit2").map(drop)
}

/// Through the first admit to the `slice` job.
fn to_slice(rig: &mut Rig) -> R {
    answer(rig, "plan", "planned", "")?;
    answer(rig, "admit1", "admitted", "")?;
    open(rig, "slice").map(drop)
}

/// The run went to the CTO from `from` with `word`, and no alarm fired.
fn at_cto(rig: &mut Rig, from: &str, word: &str) -> R {
    open(rig, "cto")?;
    assert_eq!(verdict(rig, from)?.as_deref(), Some(word));
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

/// The run parked from `from` with `word`: the park step waits, the run is
/// live, and the park step raised no step error.
fn at_parked(rig: &mut Rig, from: &str, word: &str) -> R {
    rig.tick_until("parked", |rig| rig.row("parked").is_ok_and(|r| r.is_some()))?;
    for _ in 0..3 {
        rig.tick()?;
    }
    assert_eq!(verdict(rig, from)?.as_deref(), Some(word));
    let parked = rig.row("parked")?.map(|row| (row.state, row.verdict));
    assert_eq!(parked, Some((StepState::Waiting, None)));
    assert_eq!(rig.runs()?[0].state, "live");
    assert!(!rig.exceptions().contains(STEP_ERROR_HELD));
    Ok(())
}

#[test]
fn admit1_revise_opens_the_plan_revision() -> R {
    let mut rig = build()?;
    to_plan2(&mut rig)?;
    assert_eq!(verdict(&rig, "admit1")?.as_deref(), Some("revise"));
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn admit1_escalate_goes_to_the_cto() -> R {
    let mut rig = build()?;
    answer(&mut rig, "plan", "planned", "")?;
    answer(&mut rig, "admit1", "escalate", REASON)?;
    at_cto(&mut rig, "admit1", "escalate")
}

#[test]
fn admit1_failed_goes_to_the_cto() -> R {
    let mut rig = build()?;
    answer(&mut rig, "plan", "planned", "")?;
    lose(&mut rig, "admit1")?;
    at_cto(&mut rig, "admit1", "failed")
}

#[test]
fn plan2_planned_opens_the_second_admit() -> R {
    let mut rig = build()?;
    to_admit2(&mut rig)?;
    assert_eq!(verdict(&rig, "plan2")?.as_deref(), Some("planned"));
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn plan2_delivered_lands_the_run_and_closes_the_need() -> R {
    let mut rig = build()?;
    to_plan2(&mut rig)?;
    answer(&mut rig, "plan2", "delivered", REASON)?;
    rig.tick_until("run done", |rig| {
        rig.runs()
            .is_ok_and(|runs| runs.iter().any(|run| run.state == "done"))
    })?;
    assert_eq!(verdict(&rig, "plan2")?.as_deref(), Some("delivered"));
    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Done);
    assert!(rig.row("admit2")?.is_none());
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn plan2_operator_parks_the_run() -> R {
    let mut rig = build()?;
    to_plan2(&mut rig)?;
    answer(&mut rig, "plan2", "operator", REASON)?;
    at_parked(&mut rig, "plan2", "operator")
}

#[test]
fn plan2_hold_parks_the_run() -> R {
    let mut rig = build()?;
    to_plan2(&mut rig)?;
    answer(&mut rig, "plan2", "hold", REASON)?;
    at_parked(&mut rig, "plan2", "hold")
}

#[test]
fn plan2_design_goes_to_the_cto() -> R {
    let mut rig = build()?;
    to_plan2(&mut rig)?;
    answer(&mut rig, "plan2", "design", REASON)?;
    at_cto(&mut rig, "plan2", "design")
}

#[test]
fn plan2_failed_goes_to_the_cto() -> R {
    let mut rig = build()?;
    to_plan2(&mut rig)?;
    lose(&mut rig, "plan2")?;
    at_cto(&mut rig, "plan2", "failed")
}

#[test]
fn admit2_admitted_opens_the_slicer() -> R {
    let mut rig = build()?;
    to_admit2(&mut rig)?;
    answer(&mut rig, "admit2", "admitted", "")?;
    open(&mut rig, "slice")?;
    assert_eq!(verdict(&rig, "admit2")?.as_deref(), Some("admitted"));
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn admit2_revise_goes_to_the_cto() -> R {
    let mut rig = build()?;
    to_admit2(&mut rig)?;
    answer(&mut rig, "admit2", "revise", CHANGES)?;
    at_cto(&mut rig, "admit2", "revise")
}

#[test]
fn admit2_escalate_goes_to_the_cto() -> R {
    let mut rig = build()?;
    to_admit2(&mut rig)?;
    answer(&mut rig, "admit2", "escalate", REASON)?;
    at_cto(&mut rig, "admit2", "escalate")
}

#[test]
fn admit2_failed_goes_to_the_cto() -> R {
    let mut rig = build()?;
    to_admit2(&mut rig)?;
    lose(&mut rig, "admit2")?;
    at_cto(&mut rig, "admit2", "failed")
}

#[test]
fn slice_escalate_goes_to_the_cto() -> R {
    let mut rig = build()?;
    to_slice(&mut rig)?;
    answer(&mut rig, "slice", "escalate", REASON)?;
    at_cto(&mut rig, "slice", "escalate")
}

#[test]
fn slice_failed_goes_to_the_cto() -> R {
    let mut rig = build()?;
    to_slice(&mut rig)?;
    lose(&mut rig, "slice")?;
    at_cto(&mut rig, "slice", "failed")
}

/// A sliced envelope whose one slice names an unknown target. The envelope
/// itself parses, so refusing it is the fanout validator's finding, not an
/// envelope correction.
const SLICED_NOWHERE: &str = "mode = \"slice\"\n\n[[slice]]\nid = \"a\"\ntitle = 'Build it'\n\
                              brief = 'build the thing'\ntarget = \"nowhere\"\n\
                              write_scope = [\"src/a.rs\"]\ndeps = []\n\n";

/// Drive an invalid-slice run to the held CTO, entirely through the shipped
/// handlers: the slicer answers `sliced` for a slice naming an unknown target,
/// the real fanout validator refuses it (`SLICES_INVALID`), the CTO opens, and
/// its branch-less `rework` answer is rejected before any routing effect, so
/// the CTO step waits on [`OPERATOR_HOLD`]. Asserts the absence of expansion
/// effects on the way, so every caller starts from the same produced state.
fn held_after_invalid_slices() -> Result<Rig, VivariumError> {
    let mut rig = build()?;
    to_slice(&mut rig)?;
    answer(&mut rig, "slice", "sliced", SLICED_NOWHERE)?;
    open(&mut rig, "cto")?;
    assert_eq!(verdict(&rig, "slice")?.as_deref(), Some("sliced"));
    assert_eq!(verdict(&rig, "build1")?.as_deref(), Some("failed"));
    let exceptions = rig.exceptions();
    assert_eq!(
        exceptions.matches(SLICES_INVALID).count(),
        1,
        "{exceptions}"
    );
    // The refused fanout expanded nothing: no slice links, no hand jobs, no
    // need branch, and no open fanout recorded.
    assert!(rig.store.slice_links_of_run(&rig.need, 1)?.is_empty());
    assert!(
        rig.jobs
            .jobs
            .values()
            .all(|job| job.kind != slice_dispatch::KIND)
    );
    assert!(rig.store.need_branch(&rig.need)?.is_none());
    assert!(rig.store.open_fanout(&rig.need, 1)?.is_none());
    let id = open(&mut rig, "cto")?;
    let envelope =
        "+++\nanswer = \"rework\"\nreason = 'no need branch exists to rework'\n+++\nrework";
    finish(&mut rig, &id, &agent_receipt(0, envelope))?;
    rig.tick_until("held cto", |rig| {
        rig.row("cto").is_ok_and(|row| {
            row.is_some_and(|row| {
                row.state == StepState::Waiting && row.verdict.as_deref() == Some(OPERATOR_HOLD)
            })
        })
    })?;
    Ok(rig)
}

/// A branch-less CTO `rework` is held before any restart: the run stays as it
/// was, the finished CTO job counts as applied, and ticking past the step
/// error limit changes nothing.
#[test]
fn cto_rework_after_invalid_slices_is_held_before_restart() -> R {
    let mut rig = held_after_invalid_slices()?;
    let exceptions = rig.exceptions();
    assert_eq!(exceptions.matches(WORD_REJECTED).count(), 1, "{exceptions}");
    let cto = rig.row("cto")?.expect("cto row");
    assert_eq!(cto.state, StepState::Waiting);
    assert_eq!(cto.verdict.as_deref(), Some(OPERATOR_HOLD));
    assert_eq!(rig.port.task_state(&cto.task)?.status, TaskStatus::Open);
    let cto_job = rig
        .jobs
        .jobs
        .values()
        .find(|job| job.payload.get("node").and_then(|n| n.as_str()) == Some("cto"))
        .expect("the finished cto job");
    assert_eq!(cto_job.status, JobStatus::Finished);
    assert!(cto_job.applied_at.is_some());
    assert!(rig.store.step_payload(&cto.task)?.is_none());
    let runs = rig.runs()?;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].state, "live");
    assert!(rig.row("rework1")?.is_none());
    assert!(
        rig.jobs
            .jobs
            .values()
            .all(|job| job.payload.get("node").and_then(|n| n.as_str()) != Some("rework1"))
    );

    // The hold is stable: ticking past the step error limit mints no job,
    // re-raises no alarm, and starts no rework run.
    let jobs_before = rig.jobs.jobs.len();
    for _ in 0..=STEP_ERROR_LIMIT {
        rig.tick()?;
    }
    let log = rig.exceptions();
    assert_eq!(rig.jobs.jobs.len(), jobs_before);
    assert_eq!(log.matches(WORD_REJECTED).count(), 1, "{log}");
    assert_eq!(log.matches(SLICES_INVALID).count(), 1, "{log}");
    assert!(!log.contains(RUN_ERROR), "{log}");
    assert!(!log.contains(STEP_ERROR_HELD), "{log}");
    let runs = rig.runs()?;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].state, "live");
    assert!(rig.row("rework1")?.is_none());
    Ok(())
}

/// An operator `rework` on the held branch-less CTO is refused once and
/// strands nothing: the hold stands past the step error limit with the letter
/// consumed, and a later operator `plan` on the same held task closes the old
/// CTO, supersedes the held run, and starts a new run at `plan` — with no
/// fabricated need branch, no rework row or job, and no new alarm.
#[test]
fn operator_rework_on_the_held_cto_is_refused_then_plan_recovers() -> R {
    let mut rig = held_after_invalid_slices()?;
    let held = rig.row("cto")?.expect("held cto row");
    assert_eq!(held.state, StepState::Waiting);
    assert_eq!(held.verdict.as_deref(), Some(OPERATOR_HOLD));

    // The operator answers `rework` on the held task: the answer path
    // translates the branch-preservation refusal into one alarm and records
    // the letter as applied, without moving anything else.
    let rework = rig
        .port
        .mailspace
        .pump_post_answer(None, &held.task, "rework", "still no need branch")
        .map(|posted| posted.id)?;
    let jobs_before = rig.jobs.jobs.len();
    rig.tick()?;
    let log = rig.exceptions();
    assert_eq!(log.matches(ANSWER_REFUSED).count(), 1, "{log}");
    assert!(rig.store.applied_action_ids()?.contains(&rework));
    assert_eq!(rig.jobs.jobs.len(), jobs_before);
    assert_eq!(rig.runs()?.len(), 1);
    assert!(rig.store.step_payload(&held.task)?.is_none());
    assert!(rig.row("rework1")?.is_none());
    let cto = rig.row("cto")?.expect("cto row");
    assert_eq!(cto.state, StepState::Waiting);
    assert_eq!(cto.verdict.as_deref(), Some(OPERATOR_HOLD));
    assert_eq!(rig.port.task_state(&cto.task)?.status, TaskStatus::Open);
    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Open);

    // The consumed letter is not retried: ticking past the step error limit
    // repeats no refusal, rejection or step alarm and starts no run.
    for _ in 0..=STEP_ERROR_LIMIT {
        rig.tick()?;
    }
    let log = rig.exceptions();
    assert_eq!(log.matches(ANSWER_REFUSED).count(), 1, "{log}");
    assert_eq!(log.matches(WORD_REJECTED).count(), 1, "{log}");
    assert_eq!(log.matches(SLICES_INVALID).count(), 1, "{log}");
    assert!(!log.contains(RUN_ERROR), "{log}");
    assert!(!log.contains(STEP_ERROR_HELD), "{log}");
    assert_eq!(rig.jobs.jobs.len(), jobs_before);
    let runs = rig.runs()?;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].state, "live");

    // The operator rules `plan` instead: the held CTO closes with that
    // verdict, the held run is superseded, and a new run opens at `plan`.
    let plan = rig
        .port
        .mailspace
        .pump_post_answer(None, &cto.task, "plan", "plan it again")
        .map(|posted| posted.id)?;
    let plan_job = open(&mut rig, "plan")?;
    assert!(rig.store.applied_action_ids()?.contains(&plan));
    let runs = rig.runs()?;
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].state, "superseded");
    assert_eq!(runs[1].state, "live");
    assert_eq!(runs[1].entry, "plan");
    let job = rig
        .jobs
        .jobs
        .get(&plan_job)
        .ok_or_else(|| VivariumError::Message(format!("no job {plan_job}")))?;
    assert_eq!(job.payload["run"], serde_json::json!(runs[1].run));
    let old_cto = rig
        .store
        .steps_of(&rig.need, runs[0].run)?
        .into_iter()
        .find(|row| row.node == "cto")
        .ok_or_else(|| VivariumError::Message("no old cto row".into()))?;
    assert_eq!(old_cto.state, StepState::Done);
    assert_eq!(old_cto.verdict.as_deref(), Some("plan"));
    let closed = rig.port.task_state(&old_cto.task)?;
    assert_eq!(closed.status, TaskStatus::Done);
    assert_eq!(closed.verdict.as_deref(), Some("plan"));

    // The recovery fabricated no branch and no rework step or job, and the
    // log still holds only the earlier slice and word findings.
    assert!(rig.store.need_branch(&rig.need)?.is_none());
    assert!(rig.row("rework1")?.is_none());
    for run in [runs[0].run, runs[1].run] {
        assert!(
            !rig.store
                .steps_of(&rig.need, run)?
                .iter()
                .any(|row| row.node == "rework1")
        );
    }
    assert!(
        rig.jobs
            .jobs
            .values()
            .all(|job| job.payload.get("node").and_then(|n| n.as_str()) != Some("rework1"))
    );
    let log = rig.exceptions();
    assert_eq!(log.matches(ANSWER_REFUSED).count(), 1, "{log}");
    assert_eq!(log.matches(WORD_REJECTED).count(), 1, "{log}");
    assert_eq!(log.matches(SLICES_INVALID).count(), 1, "{log}");
    assert!(!log.contains(RUN_ERROR), "{log}");
    assert!(!log.contains(STEP_ERROR_HELD), "{log}");
    Ok(())
}

/// A park row is opened `Waiting`, so the park step raises its alarm from
/// `poll` (as the lock step does), once.
#[test]
fn a_parked_run_raises_one_park_alarm() -> R {
    let mut rig = build()?;
    to_plan2(&mut rig)?;
    answer(&mut rig, "plan2", "hold", REASON)?;
    at_parked(&mut rig, "plan2", "hold")?;
    let count = rig.exceptions().matches(PARKED_ALARM).count();
    assert_eq!(count, 1, "{}", rig.exceptions());
    Ok(())
}
