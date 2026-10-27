//! End to end: a run the CTO restarts at `slice` is driven by the real slicer.
//!
//! A run restarted at `slice` has no plan of its own: the real slicer step
//! consumes the earlier run's admitted plan (the newest admitted revision),
//! freezes its provenance into the slicer job, and its `sliced` reply opens
//! the fanout for real. Without any admitted source the slice step holds for
//! the operator instead of fabricating a slicer request or restarting the
//! planner. Every test runs a build need on a real mailspace through the rig;
//! the run 1 planner, admitter, slicer and CTO steps are real pool jobs the
//! test finishes, as the pool would.

use crate::error::VivariumError;
use crate::pump::config::{PumpConfig, RungsConfig};
use crate::pump::flow::e2e_rig::{Rig, agent_receipt, rig};
use crate::pump::flow::index::StepState;
use crate::pump::flow::tick::OPERATOR_HOLD;
use crate::pump::jobs::{JobStatus, PoolJob};
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

/// The accepted document of run 1's first plan.
const PLAN_ONE: &str = "the plan: two slices on the first pass";
/// The accepted document of run 1's admitted revision.
const PLAN_TWO: &str = "the plan revision: narrower slices after the revise";
/// A newer pool reply on the accepted plan task that is not the payload.
const NEWER_REPLY: &str = "a newer pool reply that must not win";

const REVISE: &str = "+++\nanswer = \"revise\"\nreason = 'too coarse'\n+++revise the plan";
const ADMITTED: &str = "+++\nanswer = \"admitted\"\n+++\nadmitted";
const ESCALATE: &str =
    "+++\nanswer = \"escalate\"\nreason = 'the operator must decide'\n+++escalated to the cto";
const SLICED: &str = "+++\nanswer = \"sliced\"\n\n\
     [[slice]]\nid = \"a\"\ntitle = \"first\"\nbrief = 'build a'\ntarget = \"default\"\n\
     write_scope = [\"src/a.rs\"]\ndeps = []\n\n\
     [[slice]]\nid = \"b\"\ntitle = \"second\"\nbrief = 'build b on a'\ntarget = \"default\"\n\
     write_scope = [\"src/b.rs\"]\ndeps = [\"a\"]\n+++\nsliced";

fn config() -> PumpConfig {
    let mut cfg = PumpConfig::default();
    cfg.rungs = RungsConfig {
        task: Some(String::from("cargo test --lib")),
        need: Some(String::from("cargo test")),
        main: Some(String::from("cargo test --all")),
        timeout: Some(900),
        pools: vec![String::from("test-planner-pool")],
        ..RungsConfig::default()
    };
    cfg
}

fn role(name: &str, read_only: Option<bool>) -> RoutedRole {
    RoutedRole {
        name: name.into(),
        kind: Some(name.into()),
        status: String::from("active"),
        pool: Some(String::from("test-planner-pool")),
        read_only,
        model: None,
        models: Vec::new(),
    }
}

/// A build run at `entry`, with every role the front needs routed and the
/// need thread the planner and admitter briefs read.
fn build(entry: &str) -> Result<Rig, VivariumError> {
    let mut rig = rig(config(), "+++\nkind = \"build\"\n+++\n", entry)?;
    let mut routing = (*rig.ctx.routing).clone();
    routing.roles.push(role("admitter", Some(true)));
    routing.roles.push(role("slicer", Some(true)));
    routing.roles.push(role("hand", None));
    rig.ctx = rig.ctx.with_routing(routing);
    rig.registry = crate::pump::flow::handlers::shipped(&rig.ctx);
    let shown = serde_json::json!([
        {"handle": rig.need, "message_id": "m1", "from": "operator@local",
         "subject": "Need", "body": crate::letter::fixture::need_body(&["it works"], "build something in two slices")}
    ]);
    rig.project.host.reply("need show", 0, &shown.to_string());
    Ok(rig)
}

fn planned(document: &str) -> String {
    format!("+++\nanswer = \"planned\"\n+++\n{document}")
}

fn ruling(word: &str) -> String {
    format!("+++\nanswer = \"{word}\"\nreason = 'the ruling'\n+++\nruled")
}

fn design() -> String {
    String::from("+++\nanswer = \"design\"\nreason = 'a design is needed first'\n+++design")
}

/// The open jobs of `kind` right now.
fn open_of_kind(rig: &Rig, kind: &str) -> Vec<PoolJob> {
    rig.jobs
        .jobs
        .values()
        .filter(|job| matches!(job.status, JobStatus::Pending | JobStatus::Dispatched))
        .filter(|job| job.kind == kind)
        .cloned()
        .collect()
}

/// The task of run `run`'s step at `node`.
fn task_of(rig: &Rig, run: u32, node: &str) -> Result<String, VivariumError> {
    rig.store
        .steps_of(&rig.need, run)?
        .into_iter()
        .find(|row| row.node == node)
        .map(|row| row.task)
        .ok_or_else(|| VivariumError::Message(format!("run {run} has no {node} step")))
}

/// The job bound to run `run`'s slice step, whatever its status.
fn slice_job(rig: &Rig, run: u32) -> Option<PoolJob> {
    let task = task_of(rig, run, "slice").ok()?;
    rig.jobs
        .jobs
        .values()
        .find(|job| job.task.as_deref() == Some(&task))
        .cloned()
}

/// Tick until `node` has an open job, finish it with `envelope`, and post the
/// same result on the step task, as the pool does; the next step's brief
/// reads it there.
fn finish(rig: &mut Rig, node: &str, envelope: &str) -> R {
    rig.tick_until(&format!("a {node} job"), |rig| rig.open_job(node).is_some())?;
    let job = rig
        .open_job(node)
        .ok_or_else(|| VivariumError::Message(format!("no open {node} job")))?;
    finish_job(rig, &job, envelope)
}

/// Finish `job` with `envelope` and post the result on its step task.
fn finish_job(rig: &mut Rig, job: &PoolJob, envelope: &str) -> R {
    let receipt = agent_receipt(0, envelope);
    rig.finish(&job.id, JobStatus::Finished, &receipt)?;
    let task = job
        .task
        .clone()
        .ok_or_else(|| VivariumError::Message(format!("job {} has no task", job.id)))?;
    rig.port.mailspace.record_pool_note(
        &task,
        "pool_collected",
        &format!("{{\"job\":\"{}\"}}", job.id),
        &receipt,
    )
}

/// Tick until run 2 is live at the `slice` entry.
fn restarted_at_slice(rig: &mut Rig) -> R {
    rig.tick_until("run 2 live at slice", |rig| {
        rig.runs().is_ok_and(|runs| {
            runs.len() == 2 && runs[1].state == "live" && runs[1].entry == "slice"
        })
    })
}

/// The consuming run keeps no planner of its own: no planner or admitter step
/// rows and no planner or admitter payload rows.
fn assert_no_planner_of_its_own(rig: &Rig) -> R {
    let rows = rig.store.steps_of(&rig.need, 2)?;
    assert!(
        !rows
            .iter()
            .any(|row| row.node == "plan" || row.node == "admit1"),
        "{rows:?}"
    );
    let payloads = rig.store.step_payloads_of_run(&rig.need, 2)?;
    assert!(
        !payloads
            .iter()
            .any(|row| row.class == "planner" || row.class == "admitter"),
        "{payloads:?}"
    );
    Ok(())
}

/// Tick until run `run`'s real slicer job is open, then assert it freezes the
/// prior run's admission: the payload identities and the brief's source line
/// and document. Returns the job.
fn frozen_slicer_job(
    rig: &mut Rig,
    run: u32,
    plan_task: &str,
    admit_task: &str,
    document: &str,
) -> Result<PoolJob, VivariumError> {
    rig.tick_until("the restarted slicer job", |rig| {
        slice_job(rig, run).is_some()
    })?;
    let job = slice_job(rig, run)
        .ok_or_else(|| VivariumError::Message(format!("no slicer job of run {run}")))?;
    let source = run - 1;
    assert_eq!(
        job.payload["admitted_plan"],
        serde_json::json!({ "run": source, "planner": plan_task, "admitter": admit_task })
    );
    assert!(job.body.contains(&format!("run: {run}\n")), "{}", job.body);
    assert!(
        job.body.contains(&format!(
            "admitted plan source: run {source} planner task {plan_task} admitter task {admit_task}"
        )),
        "{}",
        job.body
    );
    assert!(
        job.body.contains(&format!("admitted plan:\n{document}")),
        "{}",
        job.body
    );
    Ok(job)
}

/// Run 1 plans, is admitted, its slicer escalates, and the head CTO rules a
/// slice restart.
fn escalate_to_a_slice_ruling(rig: &mut Rig) -> R {
    finish(rig, "slice", ESCALATE)?;
    finish(rig, "cto", &ruling("slice"))?;
    restarted_at_slice(rig)
}

#[test]
fn slice_restart_consumes_prior_admitted_plan() -> R {
    let mut rig = build("plan")?;
    finish(&mut rig, "plan", &planned(PLAN_ONE))?;
    finish(&mut rig, "admit1", ADMITTED)?;
    escalate_to_a_slice_ruling(&mut rig)?;

    assert_no_planner_of_its_own(&rig)?;
    let plan_task = task_of(&rig, 1, "plan")?;
    let admit_task = task_of(&rig, 1, "admit1")?;
    let job = frozen_slicer_job(&mut rig, 2, &plan_task, &admit_task, PLAN_ONE)?;

    // The real slicer reply opens the real fanout: slice a's hand job, while
    // slice b waits on it.
    finish_job(&mut rig, &job, SLICED)?;
    rig.tick_until("the fanout hand job", |rig| {
        open_of_kind(rig, "hand").len() == 1
    })?;
    let hand = &open_of_kind(&rig, "hand")[0];
    assert_eq!(hand.kind, "hand");
    assert!(hand.body.contains("build a"), "{}", hand.body);
    assert_eq!(
        rig.store.open_fanout(&rig.need, 2)?,
        Some(task_of(&rig, 2, "build1")?)
    );
    let slice = rig.row("slice")?.map(|row| (row.state, row.verdict));
    assert_eq!(
        slice,
        Some((StepState::Done, Some(String::from("sliced")))),
        "{}",
        rig.exceptions()
    );
    Ok(())
}

#[test]
fn slice_restart_consumes_the_admitted_plan2_revision() -> R {
    let mut rig = build("plan")?;
    finish(&mut rig, "plan", &planned(PLAN_ONE))?;
    finish(&mut rig, "admit1", REVISE)?;
    finish(&mut rig, "plan2", &planned(PLAN_TWO))?;
    finish(&mut rig, "admit2", ADMITTED)?;
    // A newer pool reply on the accepted plan task differs from the persisted
    // accepted payload; the frozen admission must stay authoritative.
    let plan2_task = task_of(&rig, 1, "plan2")?;
    rig.port.mailspace.record_pool_note(
        &plan2_task,
        "pool_collected",
        "{\"job\":\"newer\"}",
        &format!("+++\nanswer = \"planned\"\n+++\n{NEWER_REPLY}"),
    )?;
    escalate_to_a_slice_ruling(&mut rig)?;

    let admit2_task = task_of(&rig, 1, "admit2")?;
    let job = frozen_slicer_job(&mut rig, 2, &plan2_task, &admit2_task, PLAN_TWO)?;
    assert!(
        !job.body.contains(NEWER_REPLY),
        "the newer pool reply leaked into the brief: {}",
        job.body
    );
    Ok(())
}

#[test]
fn zz_debug_scratch() -> R {
    let mut rig = build("plan")?;
    finish(&mut rig, "plan", &design())?;
    for _ in 0..4 {
        rig.tick()?;
        eprintln!("=== after tick: jobs:");
        for (id, job) in &rig.jobs.jobs {
            eprintln!("  {id} kind={} node={:?} status={:?} applied={:?} outcome={:?}",
                job.kind,
                job.payload.get("node"),
                job.status,
                job.applied_at,
                job.outcome.as_ref().map(|o| (o.state.clone(), o.receipt.len())));
        }
        eprintln!("  steps: {:?}", rig.store.steps_of(&rig.need, 1)?);
        eprintln!("  exceptions: {}", rig.exceptions());
    }
    Ok(())
}

#[test]
fn slice_without_admitted_source_holds() -> R {
    let mut rig = build("plan")?;
    // The plan answers `design`: the CTO rules without any admission anywhere.
    finish(&mut rig, "plan", &design())?;
    finish(&mut rig, "cto", &ruling("slice"))?;
    restarted_at_slice(&mut rig)?;

    // The real slicer has no source to slice; the step holds for the operator
    // after its error budget instead of fabricating a request.
    rig.tick_until("the slice step held", |rig| {
        rig.row("slice").is_ok_and(|row| {
            row.as_ref().is_some_and(|row| {
                row.state == StepState::Waiting && row.verdict.as_deref() == Some(OPERATOR_HOLD)
            })
        })
    })?;
    assert!(
        rig.exceptions().contains("flow-step-error-held"),
        "{}",
        rig.exceptions()
    );
    assert!(
        slice_job(&rig, 2).is_none(),
        "a slicer request was fabricated"
    );
    let runs = rig.runs()?;
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[1].state, "live", "{}", rig.exceptions());
    assert!(
        !rig.store
            .steps_of(&rig.need, 2)?
            .iter()
            .any(|row| row.node == "plan"),
        "the slice restarted the planner"
    );
    Ok(())
}
