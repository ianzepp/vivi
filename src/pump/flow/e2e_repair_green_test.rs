//! End to end: the repair flow, green path, on a real mailspace.
//!
//! A repair need waits at `unrepaired` (or still works at `fix1`) while main
//! goes green. The per-target main rung tick phase schedules the main rung
//! job, a scripted receipt finishes it, and the tick lands the need: the
//! `unrepaired` step answers `green`, a run still at `fix1` is closed outside
//! the flow, and a last green tip already on main answers the poll with no new
//! main rung at all.
//!
//! It also pins what a red main rung does around a repair merge: a red on a
//! tip without the landed repair merge files nothing, and a red on a tip that
//! holds it files a new repair need.

use std::sync::Arc;

use crate::error::VivariumError;
use crate::pump::config::{PumpConfig, RungsConfig};
use crate::pump::fixture::send_need;
use crate::pump::flow::e2e_rig::{Rig, agent_receipt, push_receipt, rig};
use crate::pump::flow::index::StepState;
use crate::pump::flow::mail::{MailReader, NeedStatus, OpenRecords};
use crate::pump::flow::main_rung::{
    self, FAILING, GREEN, PAYLOAD_KEY, REPAIRED, TESTED as TESTED_KEY,
};
use crate::pump::flow::repair_unrepaired::MAIN_UNREPAIRED;
use crate::pump::jobs::{JobStatus, PoolJob};
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

const TARGET: &str = "default";
const TESTED: &str = "3333333333333333333333333333333333333333";
const MAIN_A: &str = "aaaa000000000000000000000000000000000001";
const MAIN_B: &str = "bbbb000000000000000000000000000000000002";
const MAIN_REF: &str = "main";
const FAILED: &str = "+++\nanswer = \"failed\"\n+++\ngave up";

fn record() -> String {
    format!(
        "+++\nkind = \"repair\"\ntarget = \"{TARGET}\"\ntested = \"{TESTED}\"\n+++\nFAILED a::b\n"
    )
}

/// The config with a `main` rung for the default target.
fn main_rung_config() -> PumpConfig {
    PumpConfig {
        rungs: RungsConfig {
            main: Some(String::from("cargo test")),
            timeout: Some(900),
            pools: vec![String::from("build-pool")],
            ..RungsConfig::default()
        },
        ..PumpConfig::default()
    }
}

/// A repair rig at `fix1` with main at `MAIN_A` and the first repairer job
/// open.
fn at_fix1() -> Result<Rig, VivariumError> {
    let mut rig = rig(PumpConfig::default(), &record(), "fix1")?;
    rig.git(MAIN_REF, MAIN_A);
    rig.tick_until("fix1 job", |rig| rig.open_job("fix1").is_some())?;
    Ok(rig)
}

/// Finish the open job of `node` with the repairer's `failed` envelope.
fn fail_attempt(rig: &mut Rig, node: &str) -> R {
    let job = open(rig, node)?;
    rig.finish(&job.id, JobStatus::Finished, &agent_receipt(0, FAILED))
}

fn open(rig: &Rig, node: &str) -> Result<PoolJob, VivariumError> {
    rig.open_job(node)
        .ok_or_else(|| VivariumError::Message(format!("no open job at {node}")))
}

/// A repair rig whose run waits at `unrepaired`: fix1 `failed`, then fix2
/// `fix2_word` (`failed` envelope or an `overtime` timeout), with the last
/// green tip recorded first when given.
fn at_unrepaired(fix2_word: &str, last_green: Option<&str>) -> Result<Rig, VivariumError> {
    let mut rig = at_fix1()?;
    if let Some(tip) = last_green {
        rig.store
            .set_main_state(&main_rung::key(GREEN, TARGET), tip)?;
    }
    fail_attempt(&mut rig, "fix1")?;
    rig.tick_until("fix2 job", |rig| rig.open_job("fix2").is_some())?;
    if fix2_word == "overtime" {
        let job = open(&rig, "fix2")?;
        rig.finish(&job.id, JobStatus::Timeout, "")?;
    } else {
        fail_attempt(&mut rig, "fix2")?;
    }
    wait_at_unrepaired(&mut rig)?;
    Ok(rig)
}

fn wait_at_unrepaired(rig: &mut Rig) -> R {
    rig.tick_until(
        "unrepaired",
        |rig| matches!(rig.row("unrepaired"), Ok(Some(row)) if row.state == StepState::Waiting),
    )
}

/// Turn the `main` rung on for the default target.
fn enable_main_rung(rig: &mut Rig) {
    let mut cfg = main_rung_config();
    cfg.reply_from = rig.ctx.cfg.reply_from.clone();
    rig.ctx.cfg = Arc::new(cfg);
}

/// Every main rung job the schedule enqueued, in id order.
fn main_jobs(rig: &Rig) -> Vec<PoolJob> {
    rig.jobs
        .jobs
        .values()
        .filter(|job| job.kind == "rung")
        .filter(|job| job.payload.get(PAYLOAD_KEY).and_then(|v| v.as_str()) == Some(TARGET))
        .cloned()
        .collect()
}

/// Tick until the schedule enqueued a main rung job at `tip`; return it.
fn main_job_at(rig: &mut Rig, tip: &str) -> Result<PoolJob, VivariumError> {
    rig.tick_until("main rung job", |rig| !main_jobs(rig).is_empty())?;
    let jobs = main_jobs(rig);
    let job = jobs
        .into_iter()
        .find(|job| job.payload.get("tip").and_then(|v| v.as_str()) == Some(tip))
        .ok_or_else(|| VivariumError::Message(format!("no main rung job at {tip}")))?;
    Ok(job)
}

fn run_state(rig: &Rig) -> Result<String, VivariumError> {
    let runs = rig.runs()?;
    runs.last()
        .map(|run| run.state.clone())
        .ok_or_else(|| VivariumError::Message(String::from("no run")))
}

/// Whether the need's thread holds a letter whose body contains `text`.
fn thread_mentions(rig: &Rig, text: &str) -> Result<bool, VivariumError> {
    let thread = rig.project.mailspace.thread(&rig.need, false, 200, 8)?;
    Ok(thread.iter().any(|message| message.body.contains(text)))
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
fn a_green_main_rung_answers_unrepaired_green_and_lands_the_need() -> R {
    let mut rig = at_unrepaired("failed", None)?;
    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Open);
    enable_main_rung(&mut rig);
    rig.git(MAIN_REF, MAIN_B);
    let job = main_job_at(&mut rig, MAIN_B)?;
    rig.finish(&job.id, JobStatus::Finished, &agent_receipt(0, "ok"))?;
    rig.tick_until("run done", |rig| run_state(rig).is_ok_and(|s| s == "done"))?;
    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Done);
    let unrepaired = rig.row("unrepaired")?.map(|row| row.verdict);
    assert_eq!(unrepaired, Some(Some(String::from("green"))));
    assert_eq!(
        rig.store.main_state(&main_rung::key(GREEN, TARGET))?,
        Some(MAIN_B.to_owned())
    );
    assert_eq!(main_jobs(&rig).len(), 1, "one main rung job");
    // The alarm raised while the run waited at `unrepaired`; green adds none.
    assert_eq!(kinds(&rig), [MAIN_UNREPAIRED], "{}", rig.exceptions());
    Ok(())
}

#[test]
fn a_green_main_rung_closes_a_run_still_at_fix1_and_a_late_repairer_changes_nothing() -> R {
    let mut rig = at_fix1()?;
    let repairer = open(&rig, "fix1")?;
    enable_main_rung(&mut rig);
    rig.git(MAIN_REF, MAIN_B);
    let job = main_job_at(&mut rig, MAIN_B)?;
    rig.finish(&job.id, JobStatus::Finished, &agent_receipt(0, "ok"))?;
    rig.tick_until("need closed", |rig| {
        matches!(rig.port.need_state(&rig.need), Ok(NeedStatus::Done))
    })?;
    assert!(
        thread_mentions(&rig, &format!("main green at {MAIN_B}"))?,
        "the close note names the green tip"
    );
    assert_eq!(run_state(&rig)?, "withdrawn");
    // The repairer that was still working finishes afterwards: nothing moves.
    rig.finish(&repairer.id, JobStatus::Finished, &agent_receipt(0, FAILED))?;
    rig.tick()?;
    rig.tick()?;
    assert_eq!(run_state(&rig)?, "withdrawn");
    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Done);
    assert!(
        rig.row("fix2")?.is_none(),
        "the withdrawn run starts nothing"
    );
    assert_eq!(rig.runs()?.len(), 1, "no second run");
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn a_last_green_on_the_current_main_answers_the_poll_with_no_new_main_rung() -> R {
    let mut rig = at_fix1()?;
    fail_attempt(&mut rig, "fix1")?;
    rig.tick_until("fix2 job", |rig| rig.open_job("fix2").is_some())?;
    // Main is green at its current tip, which holds the tested tip, and that
    // tip counts as tested: the schedule has nothing to start.
    enable_main_rung(&mut rig);
    rig.store
        .set_main_state(&main_rung::key(GREEN, TARGET), MAIN_A)?;
    rig.store
        .set_main_state(&main_rung::key(TESTED_KEY, TARGET), MAIN_A)?;
    fail_attempt(&mut rig, "fix2")?;
    rig.tick_until("run done", |rig| run_state(rig).is_ok_and(|s| s == "done"))?;
    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Done);
    assert!(main_jobs(&rig).is_empty(), "no main rung ran");
    assert_eq!(rig.exceptions(), "");
    let unrepaired = rig.row("unrepaired")?.map(|row| row.verdict);
    assert_eq!(unrepaired, Some(Some(String::from("green"))));
    Ok(())
}

#[test]
fn the_unrepaired_alarm_names_main_the_last_green_and_both_attempts() -> R {
    let mut rig = at_unrepaired("overtime", Some(MAIN_B))?;
    for _ in 0..3 {
        rig.tick()?;
    }
    let log = rig.exceptions();
    assert_eq!(kinds(&rig), [MAIN_UNREPAIRED], "{log}");
    assert!(log.contains(&format!("main tip {MAIN_A}")), "{log}");
    assert!(log.contains(&format!("last green {MAIN_B}")), "{log}");
    assert!(log.contains("attempt 1: failed"), "{log}");
    assert!(log.contains("attempt 2: overtime"), "{log}");
    assert!(log.contains(TESTED), "{log}");
    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Open);
    assert!(MailReader::open_record(&rig.port, &rig.need)?.is_some());
    Ok(())
}

const OLD_TIP: &str = "1111111111111111111111111111111111111111";
const CAND1: &str = "2222222222222222222222222222222222222222";
const NEW_TIP: &str = "6666666666666666666666666666666666666666";
const MERGED: &str = "5555555555555555555555555555555555555555";
const RED: &str = "test a::b ... FAILED";
const POOL: &str = "test-planner-pool";
const SEND: &str = "need send";
const ANCESTOR: &str = "merge-base --is-ancestor";
const BUILT: &str = "+++\nanswer = \"built\"\n+++\nfixed";

fn merge_config() -> PumpConfig {
    PumpConfig {
        rungs: RungsConfig {
            main: Some(String::from("cargo test")),
            timeout: Some(900),
            pools: vec![String::from(POOL)],
            ..RungsConfig::default()
        },
        ..PumpConfig::default()
    }
}

/// A repair run with attempt 1 published at `CAND1`, its rung passed and its
/// merge-main job open, while main sits at `OLD_TIP` and the main rung job at
/// that tip is open too. The record's failing set is stored as the main rung
/// would have stored it when it filed the need.
fn at_merge1() -> Result<Rig, VivariumError> {
    let mut rig = rig(merge_config(), &record(), "fix1")?;
    let mut routing = (*rig.ctx.routing).clone();
    for (name, read_only) in [("merge", false), ("auditor", true)] {
        routing.roles.push(RoutedRole {
            name: name.into(),
            kind: Some(name.into()),
            status: String::from("active"),
            pool: Some(String::from(POOL)),
            read_only: Some(read_only),
            model: None,
            models: Vec::new(),
        });
    }
    rig.ctx = rig.ctx.clone().with_routing(routing);
    rig.git(MAIN_REF, OLD_TIP);
    rig.store
        .set_main_state(&main_rung::key(FAILING, TARGET), "[\"a::b\"]")?;
    rig.tick_until("fix1 job", |rig| rig.open_job("fix1").is_some())?;
    let fix = open(&rig, "fix1")?;
    let reference = fix
        .publish_ref
        .clone()
        .ok_or_else(|| VivariumError::Message(String::from("fix job has no publish ref")))?;
    let publish = format!("runner publish: pushed {reference} {CAND1} {CAND1}");
    let receipt = push_receipt(0, "pushed", &publish, BUILT);
    rig.finish(&fix.id, JobStatus::Finished, &receipt)?;
    rig.tick_until("rung1 job", |rig| rig.open_job("rung1").is_some())?;
    let rung = open(&rig, "rung1")?;
    rig.finish(&rung.id, JobStatus::Finished, &agent_receipt(0, ""))?;
    rig.tick_until("merge1 job", |rig| rig.open_job("merge1").is_some())?;
    rig.tick_until("main rung job", |rig| !main_jobs(rig).is_empty())?;
    Ok(rig)
}

/// Finish the main rung job at `tip` red with the record's failing id.
fn finish_main_red(rig: &mut Rig, tip: &str) -> R {
    let job = main_job_at_tip(rig, tip)?;
    rig.finish(&job.id, JobStatus::Finished, &agent_receipt(101, RED))
}

fn main_job_at_tip(rig: &Rig, tip: &str) -> Result<PoolJob, VivariumError> {
    main_jobs(rig)
        .into_iter()
        .find(|job| job.payload.get("tip").and_then(|v| v.as_str()) == Some(tip))
        .ok_or_else(|| VivariumError::Message(format!("no main rung job at {tip}")))
}

/// Land the open merge1 job at `MERGED` and tick until the run is done.
fn land(rig: &mut Rig) -> R {
    let job = open(rig, "merge1")?;
    let publish = format!("runner publish: pushed refs/heads/main {MERGED} {MERGED}");
    let receipt = push_receipt(0, "", &publish, "");
    rig.finish(&job.id, JobStatus::Finished, &receipt)?;
    rig.tick_until("run done", |rig| run_state(rig).is_ok_and(|s| s == "done"))
}

fn sends(rig: &Rig) -> usize {
    rig.project
        .host
        .commands()
        .iter()
        .filter(|command| command.contains(SEND))
        .count()
}

fn audits(rig: &Rig) -> Vec<String> {
    rig.jobs
        .jobs
        .keys()
        .filter(|id| id.starts_with("repair-audit-"))
        .cloned()
        .collect()
}

#[test]
fn a_red_on_an_older_tip_while_the_repair_waits_to_merge_files_nothing() -> R {
    let mut rig = at_merge1()?;
    finish_main_red(&mut rig, OLD_TIP)?;
    for _ in 0..3 {
        rig.tick()?;
    }
    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Open);
    assert_eq!(rig.port.open_record_needs()?.len(), 1);
    assert_eq!(rig.runs()?.len(), 1, "one run");
    assert_eq!(sends(&rig), 0, "no repair need is sent");
    assert!(audits(&rig).is_empty(), "{:?}", audits(&rig));
    assert_eq!(kinds(&rig), Vec::<String>::new(), "{}", rig.exceptions());
    Ok(())
}

#[test]
fn a_red_on_a_tip_without_the_landed_merge_files_nothing() -> R {
    let mut rig = at_merge1()?;
    land(&mut rig)?;
    let landed = audits(&rig);
    assert_eq!(
        landed.len(),
        1,
        "the landing queues its own audit: {landed:?}"
    );
    assert_eq!(
        rig.store.main_state(&main_rung::key(REPAIRED, TARGET))?,
        Some(MERGED.to_owned())
    );
    // The main rung job that started at the old tip does not hold the merge.
    rig.project.host.reply(ANCESTOR, 1, "");
    finish_main_red(&mut rig, OLD_TIP)?;
    for _ in 0..3 {
        rig.tick()?;
    }
    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Done);
    assert!(rig.port.open_record_needs()?.is_empty());
    assert_eq!(rig.runs()?.len(), 1, "no new run");
    assert_eq!(sends(&rig), 0, "no repair need is sent");
    assert_eq!(audits(&rig), landed, "no new repair audit");
    assert_eq!(kinds(&rig), Vec::<String>::new(), "{}", rig.exceptions());
    Ok(())
}

#[test]
fn a_red_on_a_tip_with_the_landed_merge_files_one_new_repair_need() -> R {
    let mut rig = at_merge1()?;
    land(&mut rig)?;
    rig.project.host.reply(ANCESTOR, 1, "");
    finish_main_red(&mut rig, OLD_TIP)?;
    rig.tick()?;
    assert_eq!(sends(&rig), 0);
    // Main moves on to a tip that holds the merge; the next main rung is red
    // with the same failing ids.
    let filed = send_need(
        &rig.port.mailspace,
        "pump: main rung red on default",
        "mind",
    );
    rig.project
        .host
        .reply(SEND, 0, &format!("created need {filed}\n"));
    rig.project.host.reply(ANCESTOR, 0, "");
    rig.project.host.advance(3600);
    rig.git(MAIN_REF, NEW_TIP);
    rig.tick_until("main rung at the new tip", |rig| {
        main_job_at_tip(rig, NEW_TIP).is_ok()
    })?;
    finish_main_red(&mut rig, NEW_TIP)?;
    rig.tick_until("new repair need", |rig| sends(rig) > 0)?;
    assert_eq!(sends(&rig), 1, "one new repair need is sent");
    assert!(MailReader::open_record(&rig.port, &filed)?.is_some());
    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Done);
    assert_eq!(kinds(&rig), Vec::<String>::new(), "{}", rig.exceptions());
    Ok(())
}
