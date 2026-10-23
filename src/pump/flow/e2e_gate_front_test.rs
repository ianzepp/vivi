//! End to end gate: the research and plan front of the build flow.
//!
//! Region: `research` and `plan`. Edges covered: research `READY`, `NOT READY`,
//! `NEEDS FURTHER REVIEW` and `failed`; plan `delivered`, `operator`, `hold`,
//! `design` and `failed`, including the park node and its restart at `plan`.
//!
//! Every test runs a build need on a real mailspace through `tick_with` with
//! the shipped registry; the test only finishes the pool jobs the pump opened,
//! as the pool would, and ticks. `planned` (plan to `admit1`) is covered by
//! `e2e_gate_full_test.rs`.

use std::thread::sleep;
use std::time::Duration;

use super::e2e_rig::{Rig, agent_receipt, rig};
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::flow::index::StepState;
use crate::pump::flow::mail::{MailReader, NeedStatus};
use crate::pump::jobs::JobStatus;

type R = Result<(), VivariumError>;

/// The alarm kind the park step raises once (the park module is private).
const NEED_PARKED: &str = "flow-need-parked";

/// A build run started at `entry` on a real mailspace.
fn build(entry: &str) -> Result<Rig, VivariumError> {
    let rig = rig(PumpConfig::default(), "kind: build", entry)?;
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

/// Tick until `node` has an open job, then finish it with `envelope`.
fn answer(rig: &mut Rig, node: &str, envelope: &str) -> R {
    let id = open(rig, node)?;
    rig.finish(&id, JobStatus::Finished, &agent_receipt(0, envelope))
}

/// Lose the first two attempts of `node`: the step fails after its retry.
fn lose_twice(rig: &mut Rig, node: &str) -> R {
    for _ in 0..2 {
        let id = open(rig, node)?;
        rig.finish(&id, JobStatus::Lost, "")?;
    }
    Ok(())
}

fn researcher(word: &str) -> String {
    format!("+++\nanswer = \"{word}\"\n+++\n\n## Verdict\nAnalysis for {word}.")
}

fn planner(word: &str) -> String {
    format!("+++\nanswer = \"{word}\"\nreason = 'because {word}'\n+++\nplan says {word}")
}

fn verdict(rig: &Rig, node: &str) -> Result<Option<String>, VivariumError> {
    Ok(rig.row(node)?.and_then(|row| row.verdict))
}

fn run_done(rig: &Rig) -> bool {
    rig.runs()
        .is_ok_and(|runs| runs.iter().any(|run| run.state == "done"))
}

/// The CTO job opened after `node` answered `word`; the run is still live.
fn assert_cto(rig: &mut Rig, node: &str, word: &str) -> R {
    open(rig, "cto")?;
    assert_eq!(verdict(rig, node)?.as_deref(), Some(word));
    assert_eq!(rig.runs()?.len(), 1);
    assert_eq!(rig.runs()?[0].state, "live");
    Ok(())
}

/// Count the alarms of `kind` in the pump's exceptions file.
fn alarms(rig: &Rig, kind: &str) -> usize {
    rig.exceptions().matches(kind).count()
}

#[test]
fn a_ready_research_answer_opens_the_plan() -> R {
    let mut rig = build("research")?;
    answer(&mut rig, "research", &researcher("READY"))?;
    open(&mut rig, "plan")?;
    assert_eq!(verdict(&rig, "research")?.as_deref(), Some("READY"));
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn a_not_ready_research_answer_goes_to_the_cto() -> R {
    let mut rig = build("research")?;
    answer(&mut rig, "research", &researcher("NOT READY"))?;
    assert_cto(&mut rig, "research", "NOT READY")?;
    assert!(rig.row("plan")?.is_none());
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn a_needs_further_review_research_answer_goes_to_the_cto() -> R {
    let mut rig = build("research")?;
    answer(&mut rig, "research", &researcher("NEEDS FURTHER REVIEW"))?;
    assert_cto(&mut rig, "research", "NEEDS FURTHER REVIEW")?;
    assert!(rig.row("plan")?.is_none());
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn a_failed_research_step_goes_to_the_cto() -> R {
    let mut rig = build("research")?;
    lose_twice(&mut rig, "research")?;
    assert_cto(&mut rig, "research", "failed")?;
    assert!(rig.row("plan")?.is_none());
    Ok(())
}

#[test]
fn a_delivered_plan_lands_the_need() -> R {
    let mut rig = build("plan")?;
    answer(&mut rig, "plan", &planner("delivered"))?;
    rig.tick_until("run done", run_done)?;
    assert_eq!(verdict(&rig, "plan")?.as_deref(), Some("delivered"));
    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Done);
    assert!(rig.row("admit1")?.is_none());
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

/// A planner answer that parks the run: the park step waits, and a newer
/// reply from the operator restarts the need at `plan` as run 2.
fn parks_and_restarts(word: &str) -> R {
    let mut rig = build("plan")?;
    answer(&mut rig, "plan", &planner(word))?;
    rig.tick_until("parked", |rig| {
        rig.row("parked").is_ok_and(|row| row.is_some())
    })?;
    assert_eq!(verdict(&rig, "plan")?.as_deref(), Some(word));
    for _ in 0..2 {
        rig.tick()?;
    }
    let parked = rig.row("parked")?.map(|row| (row.state, row.verdict));
    assert_eq!(parked, Some((StepState::Waiting, None)));
    assert_eq!(rig.runs()?.len(), 1);
    assert_eq!(rig.runs()?[0].state, "live");

    // A reply must be newer than the run's start post, which has a
    // one-second date resolution.
    sleep(Duration::from_millis(1100));
    rig.port.mailspace.reply(
        &rig.need,
        "operator",
        vec!["mind".into()],
        Vec::new(),
        None,
        "actually, here is the answer to your question".into(),
    )?;
    rig.tick_until("run 2", |rig| rig.runs().is_ok_and(|runs| runs.len() == 2))?;
    let runs = rig.runs()?;
    assert_eq!(runs[0].state, "superseded");
    assert_eq!((runs[1].run, runs[1].entry.as_str()), (2, "plan"));
    assert_eq!(runs[1].state, "live");
    open(&mut rig, "plan")?;
    Ok(())
}

/// The park step is meant to raise its alarm once (see `build_front/park.rs`).
/// A park row starts Waiting, so the tick never calls the handler's `start`
/// and nothing else raises it: the alarm never fires. Remove the `ignore`
/// when the park handler raises the alarm from `poll`, as the lock step does.
#[test]
#[ignore = "defect: the park step never raises flow-need-parked (its handler only acts in start, which a Waiting park row never gets)"]
fn a_parked_run_raises_one_park_alarm() -> R {
    let mut rig = build("plan")?;
    answer(&mut rig, "plan", &planner("operator"))?;
    rig.tick_until("parked", |rig| {
        rig.row("parked").is_ok_and(|row| row.is_some())
    })?;
    for _ in 0..3 {
        rig.tick()?;
    }
    assert_eq!(alarms(&rig, NEED_PARKED), 1, "{}", rig.exceptions());
    Ok(())
}

#[test]
fn an_operator_plan_parks_the_run_and_a_reply_restarts_it_at_plan() -> R {
    parks_and_restarts("operator")
}

#[test]
fn a_hold_plan_parks_the_run_and_a_reply_restarts_it_at_plan() -> R {
    parks_and_restarts("hold")
}

#[test]
fn a_design_plan_goes_to_the_cto() -> R {
    let mut rig = build("plan")?;
    answer(&mut rig, "plan", &planner("design"))?;
    assert_cto(&mut rig, "plan", "design")?;
    assert!(rig.row("parked")?.is_none());
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn a_failed_plan_step_goes_to_the_cto() -> R {
    let mut rig = build("plan")?;
    lose_twice(&mut rig, "plan")?;
    assert_cto(&mut rig, "plan", "failed")?;
    assert!(rig.row("admit1")?.is_none());
    Ok(())
}
