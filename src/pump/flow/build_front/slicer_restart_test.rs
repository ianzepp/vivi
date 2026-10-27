//! End to end: a slice restart consumes the earlier run's admitted plan.
//!
//! Region: node `slice` of a run restarted at `slice` by a head-cto ruling,
//! and the fanout that opens from its reply. Edges covered: `slice` on a run
//! whose admission belongs to the earlier run (first plan and admitted
//! `plan2` revision variants), the actual fanout opening from the real slicer
//! envelope (no override handler), and a slice entry with no admitted source
//! reaching the step-error hold: no slicer request is queued and no plan
//! restarts.
//!
//! Every test runs a build need on a real mailspace through the rig with the
//! shipped registry; the test finishes the pool jobs the pump opens, as the
//! pool would, and posts each result on the step task, where the next
//! handler reads it.

use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::flow::e2e_rig::{Rig, agent_receipt, rig};
use crate::pump::flow::handlers::shipped;
use crate::pump::flow::index::StepState;
use crate::pump::flow::step_error::{STEP_ERROR_HELD, STEP_ERROR_LIMIT};
use crate::pump::flow::tick::OPERATOR_HOLD;
use crate::pump::jobs::{JobStatus, PoolJob};
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

/// The main tip every `ls-remote` of main answers.
const MAIN: &str = "3333333333333333333333333333333333333333";

/// The accepted document of the first plan.
const PLAN_ONE: &str = "the plan: build it in one pass";
/// The accepted document of the admitted revision.
const PLAN_TWO: &str = "the plan: build it in two slices";
/// A newer pool reply on a planner task that no admitter ever accepted.
const NEWER_PLAN: &str = "a newer plan draft that no admitter accepted";

const PLANNED_ONE: &str = "+++\nanswer = \"planned\"\n+++\nthe plan: build it in one pass";
const PLANNED_TWO: &str = "+++\nanswer = \"planned\"\n+++\nthe plan: build it in two slices";
const ADMITTED: &str = "+++\nanswer = \"admitted\"\n+++\nadmitted";
const REVISED: &str =
    "+++\nanswer = \"revise\"\nreason = 'cut it differently'\n+++cut it differently";
const ESCALATED: &str =
    "+++\nanswer = \"escalate\"\nreason = 'the cut needs a ruling'\n+++escalated";
const SLICE_RULING: &str = "+++\nanswer = \"slice\"\nreason = 'recut the work'\n+++recut the work";
const SLICED: &str = "+++\nanswer = \"sliced\"\n\n\
     [[slice]]\nid = \"a\"\ntitle = \"first\"\nbrief = 'build a'\ntarget = \"default\"\n\
     write_scope = [\"src/a.rs\"]\ndeps = []\n\n\
     [[slice]]\nid = \"b\"\ntitle = \"second\"\nbrief = 'build b on a'\ntarget = \"default\"\n\
     write_scope = [\"src/b.rs\"]\ndeps = [\"a\"]\n+++\nsliced";

/// A build run at `plan`, with the admitter, slicer and hand roles routed
/// besides the fixture's planner, and main at [`MAIN`].
fn build() -> Result<Rig, VivariumError> {
    let mut rig = rig(
        PumpConfig::default(),
        "+++\nkind = \"build\"\n+++\n",
        "plan",
    )?;
    let mut routing = (*rig.ctx.routing).clone();
    for (name, read_only) in [
        ("admitter", Some(true)),
        ("slicer", Some(true)),
        ("hand", None),
    ] {
        routing.roles.push(RoutedRole {
            name: String::from(name),
            kind: Some(String::from(name)),
            status: String::from("active"),
            pool: Some(String::from("test-planner-pool")),
            read_only,
            model: None,
            models: Vec::new(),
        });
    }
    rig.ctx = rig.ctx.with_routing(routing);
    rig.registry = shipped(&rig.ctx);
    rig.git("main", MAIN);
    let shown = serde_json::json!([
        {"handle": rig.need, "message_id": "m1", "from": "operator@local",
         "subject": "Need", "body": crate::letter::fixture::need_body(&["it works"], "Build the thing")}
    ]);
    rig.project.host.reply("need show", 0, &shown.to_string());
    Ok(rig)
}

/// Tick until `node` has an open job, finish it with `receipt`, and post the
/// same result on the step task, as the pool does; the next brief reads it
/// there.
fn finish(rig: &mut Rig, node: &str, receipt: &str) -> R {
    rig.tick_until(&format!("a {node} job"), |rig| rig.open_job(node).is_some())?;
    let id = rig
        .open_job(node)
        .ok_or_else(|| VivariumError::Message(format!("no open {node} job")))?
        .id;
    let row = rig
        .store
        .step_job(&id)?
        .ok_or_else(|| VivariumError::Message(format!("job {id} has no step row")))?;
    rig.finish(&id, JobStatus::Finished, receipt)?;
    rig.port.mailspace.record_pool_note(
        &row.task,
        "pool_collected",
        &format!("{{\"job\":\"{id}\"}}"),
        receipt,
    )?;
    Ok(())
}

/// The task of `node`'s newest row in run 1.
fn run1_task(rig: &Rig, node: &str) -> Result<String, VivariumError> {
    rig.store
        .steps_of(&rig.need, 1)?
        .into_iter()
        .rev()
        .find(|row| row.node == node)
        .map(|row| row.task)
        .ok_or_else(|| VivariumError::Message(format!("no {node} row in run 1")))
}

/// Answer the open cto job with the `slice` ruling and tick until run 2 is
/// live at the slicer.
fn rule_slice(rig: &mut Rig) -> R {
    finish(rig, "cto", &agent_receipt(0, SLICE_RULING))?;
    rig.tick_until("run 2 live at slice", |rig| {
        rig.runs().is_ok_and(|runs| {
            runs.len() == 2 && runs[1].state == "live" && runs[1].entry == "slice"
        })
    })?;
    Ok(())
}

/// The open slicer job of the restarted run 2.
fn slicer_job(rig: &mut Rig) -> Result<PoolJob, VivariumError> {
    rig.tick_until("the slicer job of run 2", |rig| {
        rig.open_job("slice").is_some()
    })?;
    rig.open_job("slice")
        .ok_or_else(|| VivariumError::Message("no slicer job for run 2".into()))
}

/// The job of run 2's slicer freezes the admission of `plan_task` and
/// `admit_task`: the source identities in the brief, the accepted document,
/// and the machine-readable provenance in the payload.
fn assert_frozen(job: &PoolJob, plan_task: &str, admit_task: &str, document: &str) -> R {
    assert!(
        job.body.contains(&format!(
            "admitted plan source: run 1 planner task {plan_task} admitter task {admit_task}"
        )),
        "{}",
        job.body
    );
    assert!(
        job.body.contains(&format!("admitted plan:\n{document}")),
        "{}",
        job.body
    );
    assert_eq!(
        job.payload["admitted_plan"],
        serde_json::json!({ "run": 1, "planner": plan_task, "admitter": admit_task })
    );
    Ok(())
}

/// The consuming run opened no planner or admitter of its own: neither a
/// step row nor a job.
fn assert_no_planner(rig: &Rig) -> R {
    let rows = rig.store.steps_of(&rig.need, 2)?;
    assert!(
        !rows
            .iter()
            .any(|row| matches!(row.node.as_str(), "plan" | "plan2" | "admit1" | "admit2")),
        "{rows:?}"
    );
    for node in ["plan", "plan2", "admit1", "admit2"] {
        assert!(rig.open_job(node).is_none(), "{node}");
    }
    Ok(())
}

/// The real fanout opened from the slicer's reply: both slices have rows in
/// run 2, the free slice is dispatched as a hand job from the recorded base
/// while the other still waits on it, build1 is running, and no alarm fired.
fn assert_fanout_opened(rig: &mut Rig) -> R {
    rig.tick_until("the hand job of the free slice", |rig| {
        rig.jobs.jobs.values().any(|job| job.kind == "hand")
    })?;
    let rows = rig.store.steps_of(&rig.need, 2)?;
    let row_of = |id: &str| {
        rows.iter()
            .find(|row| row.node == format!("slice:{id}"))
            .ok_or_else(|| VivariumError::Message(format!("no slice:{id} row in run 2")))
    };
    let a = row_of("a")?;
    let b = row_of("b")?;
    let hands: Vec<&PoolJob> = rig
        .jobs
        .jobs
        .values()
        .filter(|job| job.kind == "hand")
        .collect();
    assert_eq!(hands.len(), 1, "{:?}", rig.jobs.jobs.keys());
    assert_eq!(hands[0].task.as_deref(), Some(a.task.as_str()));
    assert_eq!(hands[0].from_ref.as_deref(), Some(MAIN));
    assert_eq!(a.state, StepState::Running);
    assert_eq!(b.state, StepState::Pending);
    let build1 = rows
        .iter()
        .find(|row| row.node == "build1")
        .ok_or_else(|| VivariumError::Message("no build1 row in run 2".into()))?;
    assert_eq!(build1.state, StepState::Running);
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn slice_restart_consumes_prior_admitted_plan() -> R {
    let mut rig = build()?;

    // Run 1 executes its planner and its admission; its slicer escalates to
    // the head cto, which rules `slice`.
    finish(&mut rig, "plan", &agent_receipt(0, PLANNED_ONE))?;
    finish(&mut rig, "admit1", &agent_receipt(0, ADMITTED))?;
    finish(&mut rig, "slice", &agent_receipt(0, ESCALATED))?;
    rule_slice(&mut rig)?;
    let plan = run1_task(&rig, "plan")?;
    let admit = run1_task(&rig, "admit1")?;

    // The restarted slicer consumes run 1's admission: no planner of its
    // own, and the frozen source in its job.
    assert_no_planner(&rig)?;
    let job = slicer_job(&mut rig)?;
    assert_frozen(&job, &plan, &admit, PLAN_ONE)?;

    // The real slicer envelope opens the real fanout: no override handler.
    finish(&mut rig, "slice", &agent_receipt(0, SLICED))?;
    assert_fanout_opened(&mut rig)?;
    Ok(())
}

#[test]
fn slice_restart_consumes_the_admitted_revision() -> R {
    let mut rig = build()?;

    // Run 1 admits a revision: plan, revise, plan2, then admit2; its slicer
    // escalates to the head cto, which rules `slice`.
    finish(&mut rig, "plan", &agent_receipt(0, PLANNED_ONE))?;
    finish(&mut rig, "admit1", &agent_receipt(0, REVISED))?;
    finish(&mut rig, "plan2", &agent_receipt(0, PLANNED_TWO))?;
    finish(&mut rig, "admit2", &agent_receipt(0, ADMITTED))?;
    finish(&mut rig, "slice", &agent_receipt(0, ESCALATED))?;

    // A newer pool reply on the admitted planner task differs from the
    // persisted accepted payload.
    let plan2 = run1_task(&rig, "plan2")?;
    let newer = agent_receipt(0, NEWER_PLAN);
    rig.port
        .mailspace
        .record_pool_note(&plan2, "pool_collected", "{\"job\":\"newer\"}", &newer)?;

    rule_slice(&mut rig)?;
    let plan = run1_task(&rig, "plan2")?;
    let admit = run1_task(&rig, "admit2")?;
    assert_no_planner(&rig)?;
    let job = slicer_job(&mut rig)?;
    assert_frozen(&job, &plan, &admit, PLAN_TWO)?;
    // The persisted accepted payload stays authoritative over the newer
    // reply and over the abandoned first plan.
    assert!(!job.body.contains(NEWER_PLAN), "{}", job.body);
    assert!(!job.body.contains(PLAN_ONE), "{}", job.body);
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn slice_without_admitted_source_holds() -> R {
    let mut rig = build()?;

    // Run 1 plans, but nothing is ever admitted: admit1 escalates to the
    // head cto, which rules `slice`.
    finish(&mut rig, "plan", &agent_receipt(0, PLANNED_ONE))?;
    finish(&mut rig, "admit1", &agent_receipt(0, ESCALATED))?;

    // A newer pool reply on the planner task differs from the persisted
    // payload; a reply is not a source either.
    let plan = run1_task(&rig, "plan")?;
    let newer = agent_receipt(0, NEWER_PLAN);
    rig.port
        .mailspace
        .record_pool_note(&plan, "pool_collected", "{\"job\":\"newer\"}", &newer)?;

    rule_slice(&mut rig)?;

    // The slicer has no admitted source: its start fails until the step is
    // held for the operator, no slicer request is queued, and no plan
    // restart happens.
    for _ in 0..STEP_ERROR_LIMIT {
        rig.tick()?;
    }
    let slice = rig
        .row("slice")?
        .ok_or_else(|| VivariumError::Message("no slice row in run 2".into()))?;
    assert_eq!(slice.state, StepState::Waiting);
    assert_eq!(slice.verdict.as_deref(), Some(OPERATOR_HOLD));
    let exceptions = rig.exceptions();
    assert_eq!(
        exceptions.matches(STEP_ERROR_HELD).count(),
        1,
        "{exceptions}"
    );
    assert!(
        exceptions.contains("no admitted plan to slice"),
        "{exceptions}"
    );
    assert!(
        !rig.jobs.jobs.values().any(|job| job.kind == "slicer"),
        "a slicer request was queued"
    );
    assert_no_planner(&rig)?;
    let runs = rig.runs()?;
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[1].state, "live");
    assert_eq!(runs[1].entry, "slice");
    Ok(())
}
