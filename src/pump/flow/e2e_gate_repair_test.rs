//! End to end gate: the repair flow edges not driven elsewhere.
//!
//! Region: `fix1`, `rung1`, `merge1`, `fix2`, `rung2`, `merge2` and the
//! unrepaired wait. Edges covered: merge2 `merged`, `failed`, `conflict`;
//! rung2 lost once, `fail`, `failed`; merge1 and rung1 failures into `fix2`.
//! The fix1/fix2 edges live in `e2e_repair_fix_test.rs` and the rung1, merge1,
//! rung2-fail and merge2-conflict edges in `e2e_repair_land_test.rs`; this file
//! adds the attempt-2 land edges and re-drives the failures into `fix2` and
//! `unrepaired` with the alarm count asserted.

use crate::error::VivariumError;
use crate::pump::config::{PumpConfig, RungsConfig};
use crate::pump::flow::e2e_rig::{Rig, agent_receipt, push_receipt, rig};
use crate::pump::flow::index::StepState;
use crate::pump::flow::mail::{MailReader, NeedStatus};
use crate::pump::jobs::{JobStatus, PoolJob};
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

const TESTED: &str = "0000000000000000000000000000000000000000";
const MAIN_A: &str = "1111111111111111111111111111111111111111";
const CAND1: &str = "2222222222222222222222222222222222222222";
const CAND2: &str = "3333333333333333333333333333333333333333";
const MERGED: &str = "5555555555555555555555555555555555555555";
const BUILT: &str = "+++\nanswer = \"built\"\n+++\nfixed";

fn config() -> PumpConfig {
    let mut cfg = PumpConfig::default();
    cfg.rungs = RungsConfig {
        main: Some(String::from("cargo test")),
        timeout: Some(900),
        pools: vec![String::from("test-planner-pool")],
        ..RungsConfig::default()
    };
    cfg
}

fn main_is(rig: &Rig, sha: &str) {
    rig.project.host.reply(
        "ls-remote origin main",
        0,
        &format!("{sha}\trefs/heads/main\n"),
    );
}

/// A repair run at `fix1` with merge and auditor roles and main at `MAIN_A`.
fn start() -> Result<Rig, VivariumError> {
    let record = format!(
        "+++\nkind = \"repair\"\ntarget = \"default\"\ntested = \"{TESTED}\"\n+++\n\nFAILED a::b\n"
    );
    let mut rig = rig(config(), &record, "fix1")?;
    let mut routing = (*rig.ctx.routing).clone();
    for (name, read_only) in [("merge", false), ("auditor", true)] {
        routing.roles.push(RoutedRole {
            name: name.into(),
            kind: Some(name.into()),
            status: String::from("active"),
            pool: Some(String::from("test-planner-pool")),
            read_only: Some(read_only),
            model: None,
            models: Vec::new(),
        });
    }
    rig.ctx = rig.ctx.clone().with_routing(routing);
    main_is(&rig, MAIN_A);
    Ok(rig)
}

fn open(rig: &Rig, node: &str) -> Result<PoolJob, VivariumError> {
    rig.open_job(node)
        .ok_or_else(|| VivariumError::Message(format!("no open job for {node}")))
}

fn verdict(rig: &Rig, node: &str) -> Result<Option<String>, VivariumError> {
    Ok(rig.row(node)?.and_then(|row| row.verdict))
}

/// Finish the open `fix<attempt>` job `built` with its own ref published at
/// `commit`, and tick until the rung job of that attempt is open.
fn publish_attempt(rig: &mut Rig, attempt: u32, commit: &str) -> R {
    let fix = format!("fix{attempt}");
    let rung = format!("rung{attempt}");
    rig.tick_until(&fix, |rig| rig.open_job(&fix).is_some())?;
    let job = open(rig, &fix)?;
    let reference = job
        .publish_ref
        .clone()
        .ok_or_else(|| VivariumError::Message("fix job has no publish ref".into()))?;
    let publish = format!("runner publish: pushed {reference} {commit} {commit}");
    let receipt = push_receipt(0, "pushed", &publish, BUILT);
    rig.finish(&job.id, JobStatus::Finished, &receipt)?;
    rig.tick_until(&rung, |rig| rig.open_job(&rung).is_some())
}

fn finish_rung(rig: &mut Rig, attempt: u32, exit: i32) -> R {
    let job = open(rig, &format!("rung{attempt}"))?;
    rig.finish(&job.id, JobStatus::Finished, &agent_receipt(exit, ""))
}

/// Attempt 1 failed its rung; attempt 2 published at `CAND2`, rung2 job open.
fn at_rung2() -> Result<Rig, VivariumError> {
    let mut rig = start()?;
    publish_attempt(&mut rig, 1, CAND1)?;
    finish_rung(&mut rig, 1, 1)?;
    publish_attempt(&mut rig, 2, CAND2)?;
    Ok(rig)
}

/// Attempt 2 published and its rung passed, merge2 job open.
fn at_merge2() -> Result<Rig, VivariumError> {
    let mut rig = at_rung2()?;
    finish_rung(&mut rig, 2, 0)?;
    rig.tick_until("merge2", |rig| rig.open_job("merge2").is_some())?;
    Ok(rig)
}

fn merged_receipt() -> String {
    let publish = format!("runner publish: pushed refs/heads/main {MERGED} {MERGED}");
    push_receipt(0, "", &publish, "")
}

fn moved_receipt() -> String {
    let publish = format!("runner publish: not_ancestor refs/heads/main {MERGED} ");
    push_receipt(0, "", &publish, "")
}

/// Tick until the unrepaired step waits, then twice more; assert the run is
/// still live, the need and its record are open and the alarm fired once.
fn assert_unrepaired(rig: &mut Rig) -> R {
    rig.tick_until(
        "unrepaired",
        |rig| matches!(rig.row("unrepaired"), Ok(Some(row)) if row.state == StepState::Waiting),
    )?;
    rig.tick()?;
    rig.tick()?;
    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Open);
    assert_eq!(rig.runs()?[0].state, "live");
    assert!(MailReader::open_record(&rig.port, &rig.need)?.is_some());
    let text = rig.exceptions();
    assert_eq!(text.matches("flow-main-unrepaired").count(), 1, "{text}");
    Ok(())
}

#[test]
fn fix2_built_rung2_pass_starts_merge2_at_the_second_candidate() -> R {
    let rig = at_merge2()?;
    assert_eq!(verdict(&rig, "fix2")?.as_deref(), Some("built"));
    assert_eq!(verdict(&rig, "rung2")?.as_deref(), Some("pass"));
    let merge = open(&rig, "merge2")?;
    assert_eq!(merge.publish_contains.as_deref(), Some(CAND2));
    assert_eq!(merge.from_ref.as_deref(), Some(MAIN_A));
    assert_eq!(merge.publish_ref.as_deref(), Some("refs/heads/main"));
    assert!(rig.open_job("merge1").is_none());
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn merge2_merged_lands_the_need_and_queues_the_repair_audit() -> R {
    let mut rig = at_merge2()?;
    let job = open(&rig, "merge2")?;
    rig.finish(&job.id, JobStatus::Finished, &merged_receipt())?;
    rig.tick_until(
        "run done",
        |rig| matches!(rig.runs(), Ok(runs) if runs.iter().any(|run| run.state == "done")),
    )?;
    assert_eq!(verdict(&rig, "merge2")?.as_deref(), Some("merged"));
    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Done);
    let outcome = rig.port.last_closed_record(&rig.need)?;
    assert!(
        outcome.is_some_and(|text| text.contains("landed")),
        "the pump record closed landed"
    );
    assert!(
        rig.jobs
            .jobs
            .contains_key(&format!("repair-audit-{}-r1", rig.need)),
        "the landed repair is queued for its audit"
    );
    assert!(rig.row("unrepaired")?.is_none());
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn merge2_failed_is_unrepaired_with_one_alarm() -> R {
    let mut rig = at_merge2()?;
    let job = open(&rig, "merge2")?;
    main_is(&rig, MAIN_A);
    rig.project.host.reply("cat-file", 128, "");
    rig.finish(&job.id, JobStatus::Lost, "")?;
    assert_unrepaired(&mut rig)?;
    assert_eq!(verdict(&rig, "merge2")?.as_deref(), Some("failed"));
    Ok(())
}

#[test]
fn merge2_with_main_moved_is_unrepaired_with_one_alarm() -> R {
    let mut rig = at_merge2()?;
    let job = open(&rig, "merge2")?;
    rig.finish(&job.id, JobStatus::Finished, &moved_receipt())?;
    assert_unrepaired(&mut rig)?;
    assert_eq!(verdict(&rig, "merge2")?.as_deref(), Some("conflict"));
    Ok(())
}

#[test]
fn rung2_exit_1_is_unrepaired_with_one_alarm() -> R {
    let mut rig = at_rung2()?;
    finish_rung(&mut rig, 2, 1)?;
    assert_unrepaired(&mut rig)?;
    assert_eq!(verdict(&rig, "rung2")?.as_deref(), Some("fail"));
    assert!(rig.open_job("merge2").is_none());
    Ok(())
}

#[test]
fn rung2_lost_once_retries_and_the_retry_result_routes() -> R {
    let mut rig = at_rung2()?;
    let first = open(&rig, "rung2")?;
    rig.finish(&first.id, JobStatus::Lost, "")?;
    rig.tick_until("rung2 retry", |rig| {
        rig.open_job("rung2").is_some_and(|job| job.id != first.id)
    })?;
    assert!(rig.row("unrepaired")?.is_none());
    finish_rung(&mut rig, 2, 0)?;
    rig.tick_until("merge2", |rig| rig.open_job("merge2").is_some())?;
    assert_eq!(verdict(&rig, "rung2")?.as_deref(), Some("pass"));
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn rung2_lost_twice_answers_failed_and_is_unrepaired() -> R {
    let mut rig = at_rung2()?;
    let first = open(&rig, "rung2")?;
    rig.finish(&first.id, JobStatus::Lost, "")?;
    rig.tick_until("rung2 retry", |rig| {
        rig.open_job("rung2").is_some_and(|job| job.id != first.id)
    })?;
    let second = open(&rig, "rung2")?;
    rig.finish(&second.id, JobStatus::Lost, "")?;
    assert_unrepaired(&mut rig)?;
    assert_eq!(verdict(&rig, "rung2")?.as_deref(), Some("failed"));
    assert!(rig.open_job("merge2").is_none());
    Ok(())
}
