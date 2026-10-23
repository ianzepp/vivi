//! End to end: the repair flow, fix path, on a real mailspace. One test per
//! edge out of `fix1` and `fix2`, driven only by finishing the repairer job
//! and ticking.

use super::e2e_rig::{Rig, agent_receipt, env_receipt, push_receipt, rig};
use crate::error::VivariumError;
use crate::pump::config::{PumpConfig, RungsConfig};
use crate::pump::flow::index::StepState;
use crate::pump::flow::repair;
use crate::pump::jobs::{JobStatus, PoolJob};

type R = Result<(), VivariumError>;

const MAIN: &str = "1111111111111111111111111111111111111111";
const COMMIT: &str = "2222222222222222222222222222222222222222";
const TESTED: &str = "0000000000000000000000000000000000000000";
const BUILT: &str = "+++\nanswer = \"built\"\n+++\nfixed";
const FAILED: &str = "+++\nanswer = \"failed\"\n+++\ngave up";

/// A repair run at `fix1` with a `main` rung command, main at [`MAIN`], and
/// the `fix1` job queued.
fn repair_rig() -> Result<Rig, VivariumError> {
    let mut cfg = PumpConfig::default();
    cfg.rungs = RungsConfig {
        main: Some(String::from("cargo test")),
        timeout: Some(900),
        pools: vec![String::from("build-pool")],
        ..RungsConfig::default()
    };
    let record = format!(
        "+++\nkind = \"repair\"\ntarget = \"default\"\ntested = \"{TESTED}\"\n+++\nFAILED a::b\n"
    );
    let mut rig = rig(cfg, &record, "fix1")?;
    rig.git("main", MAIN);
    rig.tick_until("fix1 job", |rig| rig.open_job("fix1").is_some())?;
    Ok(rig)
}

fn open(rig: &Rig, node: &str) -> Result<PoolJob, VivariumError> {
    rig.open_job(node)
        .ok_or_else(|| VivariumError::Message(format!("no open {node} job")))
}

/// Finish the open job of `node` with `status` and `receipt`.
fn end(rig: &mut Rig, node: &str, status: JobStatus, receipt: &str) -> R {
    let id = open(rig, node)?.id;
    rig.finish(&id, status, receipt)
}

/// The runner receipt of attempt `attempt` published at [`COMMIT`].
fn published(rig: &Rig, attempt: u32) -> String {
    let branch = repair::branch(&rig.need, 1, attempt);
    push_receipt(
        0,
        "",
        &format!("runner publish: pushed refs/heads/{branch} {COMMIT} {COMMIT}"),
        BUILT,
    )
}

/// Fail `fix1` with `status` and `receipt`, tick until `fix2` is queued.
fn fix2_after(status: JobStatus, receipt: &str) -> Result<Rig, VivariumError> {
    let mut rig = repair_rig()?;
    end(&mut rig, "fix1", status, receipt)?;
    rig.tick_until("fix2 job", |rig| rig.open_job("fix2").is_some())?;
    Ok(rig)
}

fn verdict(rig: &Rig, node: &str) -> Result<Option<String>, VivariumError> {
    Ok(rig.row(node)?.and_then(|row| row.verdict))
}

/// Assert the run sits at `unrepaired` with one alarm after more ticks.
fn assert_unrepaired(rig: &mut Rig) -> R {
    rig.tick_until("unrepaired", |rig| {
        matches!(rig.row("unrepaired"), Ok(Some(_)))
    })?;
    rig.tick()?;
    rig.tick()?;
    let row = rig.row("unrepaired")?.expect("unrepaired row");
    assert_eq!(row.state, StepState::Waiting);
    assert!(rig.open_job("rung2").is_none());
    let text = rig.exceptions();
    assert_eq!(text.matches("flow-main-unrepaired").count(), 1, "{text}");
    assert_eq!(rig.runs()?[0].state, "live");
    Ok(())
}

#[test]
fn fix1_built_with_a_publish_starts_the_rung_at_the_published_commit() -> R {
    let mut rig = repair_rig()?;
    let job = open(&rig, "fix1")?;
    let branch = repair::branch(&rig.need, 1, 1);
    assert_eq!(job.from_ref.as_deref(), Some(MAIN));
    assert_eq!(job.branch.as_deref(), Some(branch.as_str()));
    let receipt = published(&rig, 1);
    end(&mut rig, "fix1", JobStatus::Finished, &receipt)?;
    rig.tick_until("rung1 job", |rig| rig.open_job("rung1").is_some())?;
    assert_eq!(verdict(&rig, "fix1")?.as_deref(), Some("built"));
    let rung = open(&rig, "rung1")?;
    assert_eq!(rung.from_ref.as_deref(), Some(COMMIT));
    assert_eq!(rung.command.as_deref(), Some("cargo test"));
    assert!(rig.open_job("fix2").is_none());
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn fix1_failed_starts_the_second_attempt() -> R {
    let rig = fix2_after(JobStatus::Finished, &agent_receipt(0, FAILED))?;
    assert_eq!(verdict(&rig, "fix1")?.as_deref(), Some("failed"));
    let job = open(&rig, "fix2")?;
    let branch = repair::branch(&rig.need, 1, 2);
    assert_eq!(job.branch.as_deref(), Some(branch.as_str()));
    assert!(rig.open_job("rung1").is_none());
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn fix1_overtime_starts_the_second_attempt() -> R {
    let rig = fix2_after(JobStatus::Timeout, "")?;
    assert_eq!(verdict(&rig, "fix1")?.as_deref(), Some("overtime"));
    assert!(rig.open_job("rung1").is_none());
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn fix1_unchanged_starts_the_second_attempt_and_says_why() -> R {
    let rig = fix2_after(
        JobStatus::Finished,
        &push_receipt(0, "no_changes", "", BUILT),
    )?;
    assert_eq!(verdict(&rig, "fix1")?.as_deref(), Some("unchanged"));
    let body = open(&rig, "fix2")?.body;
    assert!(body.contains("Prior attempt"), "{body}");
    assert!(body.contains("verdict unchanged"), "{body}");
    assert!(body.contains("no_changes"), "{body}");
    assert!(body.contains("Attempt 1 published nothing."), "{body}");
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn fix2_failed_leaves_the_run_at_unrepaired() -> R {
    let mut rig = fix2_after(JobStatus::Finished, &agent_receipt(0, FAILED))?;
    end(
        &mut rig,
        "fix2",
        JobStatus::Finished,
        &agent_receipt(0, FAILED),
    )?;
    assert_unrepaired(&mut rig)?;
    assert_eq!(verdict(&rig, "fix2")?.as_deref(), Some("failed"));
    Ok(())
}

#[test]
fn fix2_overtime_leaves_the_run_at_unrepaired() -> R {
    let mut rig = fix2_after(JobStatus::Finished, &agent_receipt(0, FAILED))?;
    end(&mut rig, "fix2", JobStatus::Timeout, "")?;
    assert_unrepaired(&mut rig)?;
    assert_eq!(verdict(&rig, "fix2")?.as_deref(), Some("overtime"));
    Ok(())
}

#[test]
fn fix2_unchanged_leaves_the_run_at_unrepaired() -> R {
    let mut rig = fix2_after(JobStatus::Finished, &agent_receipt(0, FAILED))?;
    let empty = push_receipt(0, "no_changes", "", BUILT);
    end(&mut rig, "fix2", JobStatus::Finished, &empty)?;
    assert_unrepaired(&mut rig)?;
    assert_eq!(verdict(&rig, "fix2")?.as_deref(), Some("unchanged"));
    Ok(())
}

#[test]
fn an_agent_that_cannot_run_holds_fix1_with_one_alarm_and_no_second_attempt() -> R {
    let mut rig = repair_rig()?;
    end(&mut rig, "fix1", JobStatus::Finished, &env_receipt("agent"))?;
    rig.tick()?;
    rig.tick()?;
    rig.tick()?;
    let row = rig.row("fix1")?.expect("fix1 row");
    assert_eq!(row.state, StepState::Waiting);
    assert_eq!(row.verdict.as_deref(), Some("operator-hold"));
    assert!(rig.row("fix2")?.is_none());
    assert!(rig.open_job("fix2").is_none());
    let text = rig.exceptions();
    assert_eq!(text.matches("flow-env-failure").count(), 1, "{text}");
    Ok(())
}

#[test]
fn a_lost_fix1_job_reruns_on_a_fresh_ref() -> R {
    let mut rig = repair_rig()?;
    let first = open(&rig, "fix1")?;
    let branch = repair::branch(&rig.need, 1, 1);
    end(&mut rig, "fix1", JobStatus::Lost, "")?;
    rig.tick_until("rerun", |rig| {
        rig.open_job("fix1").is_some_and(|job| job.id != first.id)
    })?;
    let rerun = open(&rig, "fix1")?;
    let fresh = format!("{branch}-t2");
    assert_eq!(rerun.branch.as_deref(), Some(fresh.as_str()));
    assert_eq!(
        rerun.publish_ref.as_deref(),
        Some(format!("refs/heads/{fresh}").as_str())
    );
    assert_eq!(
        first.publish_ref.as_deref(),
        Some(format!("refs/heads/{branch}").as_str())
    );
    assert_eq!(rig.row("fix1")?.expect("fix1").state, StepState::Running);
    assert!(rig.open_job("fix2").is_none());
    assert_eq!(rig.exceptions(), "");
    Ok(())
}
