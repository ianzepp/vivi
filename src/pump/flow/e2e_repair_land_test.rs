//! End to end: the repair flow, land path, on a real mailspace. One test per
//! outcome edge out of the rung and merge steps of attempts 1 and 2 in
//! `flows/repair.mmd`, each driven only by finishing the pool job and ticking.

use crate::error::VivariumError;
use crate::letter::records::NoteWord;
use crate::pump::config::{PumpConfig, RungsConfig};
use crate::pump::flow::e2e_rig::{Rig, agent_receipt, env_receipt, push_receipt, rig};
use crate::pump::flow::index::StepState;
use crate::pump::flow::mail::{MailReader, NeedStatus};
use crate::pump::flow::slice_dispatch::SliceMail;
use crate::pump::flow::tick::OPERATOR_HOLD;
use crate::pump::jobs::JobStatus;
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

/// A repair run at `fix1` with a repairer, merge and auditor roles and a main
/// at `MAIN_A`.
fn start() -> Result<Rig, VivariumError> {
    let record = format!("kind: repair\ntarget: default\ntested: {TESTED}\n\nFAILED a::b\n");
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

fn main_is(rig: &Rig, sha: &str) {
    rig.project.host.reply(
        "ls-remote origin main",
        0,
        &format!("{sha}\trefs/heads/main\n"),
    );
}

fn open(rig: &Rig, node: &str) -> Result<crate::pump::jobs::PoolJob, VivariumError> {
    rig.open_job(node)
        .ok_or_else(|| VivariumError::Message(format!("no open job for {node}")))
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

/// Finish the open rung job of `attempt` with `exit`.
fn finish_rung(rig: &mut Rig, attempt: u32, exit: i32) -> R {
    let job = open(rig, &format!("rung{attempt}"))?;
    rig.finish(&job.id, JobStatus::Finished, &agent_receipt(exit, ""))
}

/// Attempt 1 published at `CAND1`, rung1 passed, merge1 job open.
fn at_merge1() -> Result<Rig, VivariumError> {
    let mut rig = start()?;
    publish_attempt(&mut rig, 1, CAND1)?;
    finish_rung(&mut rig, 1, 0)?;
    rig.tick_until("merge1", |rig| rig.open_job("merge1").is_some())?;
    Ok(rig)
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

fn verdict(rig: &Rig, node: &str) -> Result<Option<String>, VivariumError> {
    Ok(rig.row(node)?.and_then(|row| row.verdict))
}

fn reason(rig: &Rig, node: &str) -> Result<String, VivariumError> {
    let row = rig
        .row(node)?
        .ok_or_else(|| VivariumError::Message(format!("no step {node}")))?;
    let payload = rig
        .store
        .step_payload(&row.task)?
        .ok_or_else(|| VivariumError::Message(format!("no payload for {node}")))?;
    Ok(payload.payload["reason"]
        .as_str()
        .unwrap_or_default()
        .to_owned())
}

fn conflict_receipt() -> String {
    push_receipt(1, "", "", "CONFLICT (content): merge conflict in a.rs")
}

fn moved_receipt() -> String {
    let publish = format!("runner publish: not_ancestor refs/heads/main {MERGED} ");
    push_receipt(0, "", &publish, "")
}

fn merged_receipt() -> String {
    let publish = format!("runner publish: pushed refs/heads/main {MERGED} {MERGED}");
    push_receipt(0, "", &publish, "")
}

/// The sorted exception kinds raised so far: the second field of every
/// `exceptions.txt` line.
fn kinds(rig: &Rig) -> Vec<String> {
    let mut kinds: Vec<String> = rig
        .exceptions()
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .map(str::to_owned)
        .collect();
    kinds.sort();
    kinds
}

#[test]
fn rung1_pass_starts_merge1_at_the_tested_commit() -> R {
    let rig = at_merge1()?;
    assert_eq!(verdict(&rig, "rung1")?.as_deref(), Some("pass"));
    let merge = open(&rig, "merge1")?;
    assert_eq!(merge.publish_contains.as_deref(), Some(CAND1));
    assert_eq!(merge.from_ref.as_deref(), Some(MAIN_A));
    assert_eq!(merge.publish_ref.as_deref(), Some("refs/heads/main"));
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn rung1_exit_1_starts_fix2() -> R {
    let mut rig = start()?;
    publish_attempt(&mut rig, 1, CAND1)?;
    finish_rung(&mut rig, 1, 1)?;
    rig.tick_until("fix2", |rig| rig.open_job("fix2").is_some())?;
    assert_eq!(verdict(&rig, "rung1")?.as_deref(), Some("fail"));
    assert!(
        rig.row("merge1")?
            .is_none_or(|row| row.state == StepState::Pending)
    );
    assert!(rig.open_job("merge1").is_none());
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn rung1_lost_twice_answers_failed_and_starts_fix2() -> R {
    let mut rig = start()?;
    publish_attempt(&mut rig, 1, CAND1)?;
    let first = open(&rig, "rung1")?;
    rig.finish(&first.id, JobStatus::Lost, "")?;
    rig.tick_until("rung1 retry", |rig| {
        rig.open_job("rung1").is_some_and(|job| job.id != first.id)
    })?;
    let second = open(&rig, "rung1")?;
    rig.finish(&second.id, JobStatus::Lost, "")?;
    rig.tick_until("fix2", |rig| rig.open_job("fix2").is_some())?;
    assert_eq!(verdict(&rig, "rung1")?.as_deref(), Some("failed"));
    assert!(rig.open_job("merge1").is_none());
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn rung1_exit_127_holds_the_rung_with_one_alarm_and_no_verdict() -> R {
    let mut rig = start()?;
    publish_attempt(&mut rig, 1, CAND1)?;
    let job = open(&rig, "rung1")?;
    rig.finish(&job.id, JobStatus::Finished, &env_receipt("cargo"))?;
    for _ in 0..3 {
        rig.tick()?;
    }
    let rung = rig
        .row("rung1")?
        .ok_or_else(|| VivariumError::Message("no rung1 row".into()))?;
    assert_eq!(rung.state, StepState::Waiting);
    assert_eq!(rung.verdict.as_deref(), Some(OPERATOR_HOLD));
    let replies = rig
        .port
        .notes_from(&rung.task, NoteWord::RepairRung, "mind")?;
    assert!(replies.is_empty(), "{replies:?}");
    let text = rig.exceptions();
    assert_eq!(kinds(&rig), ["flow-env-failure"], "{text}");
    assert!(rig.open_job("fix2").is_none());
    assert!(rig.open_job("merge1").is_none());
    let rung_jobs = rig
        .jobs
        .jobs
        .values()
        .filter(|job| job.payload.get("node").and_then(|n| n.as_str()) == Some("rung1"))
        .count();
    assert_eq!(rung_jobs, 1, "the held rung job is not queued again");
    Ok(())
}

#[test]
fn merge1_merged_lands_the_need() -> R {
    let mut rig = at_merge1()?;
    let job = open(&rig, "merge1")?;
    rig.finish(&job.id, JobStatus::Finished, &merged_receipt())?;
    rig.tick_until(
        "run done",
        |rig| matches!(rig.runs(), Ok(runs) if runs.iter().any(|run| run.state == "done")),
    )?;
    assert_eq!(verdict(&rig, "merge1")?.as_deref(), Some("merged"));
    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Done);
    let outcome = rig.port.last_closed_record(&rig.need)?;
    assert!(
        outcome.is_some_and(|text| text.contains("landed")),
        "the pump record closed landed"
    );
    assert!(rig.open_job("fix2").is_none());
    assert!(
        rig.jobs
            .jobs
            .contains_key(&format!("repair-audit-{}-r1", rig.need)),
        "the landed repair is queued for its audit"
    );
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn merge1_conflict_starts_fix2() -> R {
    let mut rig = at_merge1()?;
    let job = open(&rig, "merge1")?;
    rig.finish(&job.id, JobStatus::Finished, &conflict_receipt())?;
    rig.tick_until("fix2", |rig| rig.open_job("fix2").is_some())?;
    assert_eq!(verdict(&rig, "merge1")?.as_deref(), Some("conflict"));
    assert_eq!(reason(&rig, "merge1")?, "merge conflict");
    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Open);
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn merge1_not_ancestor_means_main_moved_and_starts_fix2() -> R {
    let mut rig = at_merge1()?;
    let job = open(&rig, "merge1")?;
    rig.finish(&job.id, JobStatus::Finished, &moved_receipt())?;
    rig.tick_until("fix2", |rig| rig.open_job("fix2").is_some())?;
    assert_eq!(verdict(&rig, "merge1")?.as_deref(), Some("conflict"));
    assert_eq!(reason(&rig, "merge1")?, "main moved");
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn merge1_lost_with_main_unmoved_answers_failed_and_starts_fix2() -> R {
    let mut rig = at_merge1()?;
    let job = open(&rig, "merge1")?;
    main_is(&rig, MAIN_A);
    rig.project.host.reply("cat-file", 128, "");
    rig.finish(&job.id, JobStatus::Lost, "")?;
    rig.tick_until("fix2", |rig| rig.open_job("fix2").is_some())?;
    assert_eq!(verdict(&rig, "merge1")?.as_deref(), Some("failed"));
    assert!(reason(&rig, "merge1")?.contains(&format!("actual: main {MAIN_A} without it")));
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn rung2_exit_1_is_unrepaired() -> R {
    let mut rig = at_rung2()?;
    finish_rung(&mut rig, 2, 1)?;
    rig.tick_until(
        "unrepaired",
        |rig| matches!(rig.row("unrepaired"), Ok(Some(row)) if row.state != StepState::Pending),
    )?;
    assert_eq!(verdict(&rig, "rung2")?.as_deref(), Some("fail"));
    assert!(rig.open_job("merge2").is_none());
    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Open);
    assert_eq!(
        kinds(&rig),
        ["flow-main-unrepaired"],
        "{}",
        rig.exceptions()
    );
    Ok(())
}

#[test]
fn merge2_conflict_is_unrepaired() -> R {
    let mut rig = at_merge2()?;
    let job = open(&rig, "merge2")?;
    rig.finish(&job.id, JobStatus::Finished, &conflict_receipt())?;
    rig.tick_until(
        "unrepaired",
        |rig| matches!(rig.row("unrepaired"), Ok(Some(row)) if row.state != StepState::Pending),
    )?;
    assert_eq!(verdict(&rig, "merge2")?.as_deref(), Some("conflict"));
    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Open);
    assert_eq!(
        kinds(&rig),
        ["flow-main-unrepaired"],
        "{}",
        rig.exceptions()
    );
    Ok(())
}
