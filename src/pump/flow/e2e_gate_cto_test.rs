//! End to end gate: every CTO answer.
//!
//! Region: the `cto` step. Edges covered: `research`, `plan`, `slice` and
//! `rework`, each ending the run restarted with run r2 at the matching entry
//! (one driven on to done), and `failed` locking the run.
//!
//! Every test runs on a real mailspace through the rig. The run starts at
//! `rework1`, the second verify pass blocks (audit `block_range`, need rung
//! passing), and the join sends the run to the `cto` step; the fanout is
//! replaced by a handler that answers `built` at once.

use super::e2e_rig::{Rig, agent_receipt, push_receipt, rig};
use crate::error::VivariumError;
use crate::pump::config::{PumpConfig, RungsConfig};
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::handlers::shipped;
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::flow::mail::{MailReader, NeedStatus};
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::jobs::JobStatus;
use crate::pump::store::NeedBranchRow;
use crate::pump::store::step_payload::StepPayloadRow;
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

/// The main tip the need branch was cut from.
const BASE: &str = "1111111111111111111111111111111111111111";
/// The need branch tip: ahead of [`BASE`].
const TIP: &str = "2222222222222222222222222222222222222222";
/// The current tip of main.
const MAIN: &str = "3333333333333333333333333333333333333333";
/// The commit a merge publishes.
const MERGED: &str = "4444444444444444444444444444444444444444";

const LOCK_ALARM: &str = "flow-need-locked";

const SLICED: &str = "+++\nanswer = \"sliced\"\nmode = \"rework\"\n\n[[slice]]\nid = \"fix-1\"\n\
                      title = 'Fix it'\nbrief = 'fix the finding'\ntarget = \"default\"\n\
                      write_scope = [\"src/a.rs\"]\n+++\ncut one rework slice";
const BLOCK_RANGE: &str = "+++\nanswer = \"block_range\"\n+++\nthe range is still broken";
const CLEAN: &str = "+++\nanswer = \"clean_pass\"\n+++\nclean";

fn config() -> PumpConfig {
    let mut cfg = PumpConfig::default();
    cfg.rungs = RungsConfig {
        need: Some(String::from("cargo test")),
        main: Some(String::from("cargo test --all")),
        timeout: Some(900),
        pools: vec![String::from("test-planner-pool")],
        ..RungsConfig::default()
    };
    cfg
}

/// Stands in for the fanout handler: every fanout step is built at once.
struct BuiltAtOnce;

impl StepHandler for BuiltAtOnce {
    fn start(&self, _cx: &StartCx<'_>, _step: &StepRow) -> Result<StartAction, VivariumError> {
        Ok(StartAction::Decide {
            word: "built".into(),
        })
    }
}

/// A build run started at `rework1`, over a live need branch at [`TIP`] cut
/// from [`BASE`], with main at [`MAIN`] and a merge role routed.
fn build() -> Result<Rig, VivariumError> {
    let mut rig = rig(config(), "+++\nkind = \"build\"\n+++\n", "rework1")?;
    let mut routing = (*rig.ctx.routing).clone();
    routing.roles.push(RoutedRole {
        name: String::from("merge"),
        kind: Some(String::from("merge")),
        status: String::from("active"),
        pool: Some(String::from("test-planner-pool")),
        read_only: Some(false),
        model: None,
        models: Vec::new(),
    });
    rig.ctx = rig.ctx.with_routing(routing);
    rig.registry = shipped(&rig.ctx);
    rig.registry
        .handler(FlowKind::Build, NodeClass::Fanout, Box::new(BuiltAtOnce));
    rig.store.upsert_need_branch(&NeedBranchRow {
        need: rig.need.clone(),
        branch: format!("vivi/need/{}", rig.need),
        base: Some(BASE.into()),
        status: "live".into(),
        run: 1,
        target: "default".into(),
        priors: Vec::new(),
    })?;
    let need_ref = format!("refs/heads/vivi/need/{}", rig.need);
    rig.git(&need_ref, TIP);
    rig.git("main", MAIN);
    let shown = serde_json::json!([
        {"handle": rig.need, "message_id": "m1", "from": "operator@local",
         "subject": "Need", "body": crate::letter::fixture::need_body(&["it works"], "Build the thing")}
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

/// Tick until `node` has an open job, then finish it with `receipt`.
fn finish(rig: &mut Rig, node: &str, receipt: &str) -> R {
    let id = open(rig, node)?;
    rig.finish(&id, JobStatus::Finished, receipt)
}

/// Drive the run from `rework1` through a blocked second pass to the open
/// `cto` job.
fn to_cto(rig: &mut Rig) -> R {
    finish(rig, "rework1", &agent_receipt(0, SLICED))?;
    rig.tick_until("the audit2 and test2 jobs", |rig| {
        rig.open_job("audit2").is_some() && rig.open_job("test2").is_some()
    })?;
    finish(rig, "audit2", &agent_receipt(0, BLOCK_RANGE))?;
    finish(rig, "test2", &agent_receipt(0, "all tests passed"))?;
    open(rig, "cto").map(drop)
}

/// A head-cto envelope with `word`.
fn ruling(word: &str) -> String {
    format!("+++\nanswer = \"{word}\"\nreason = 'the ruling'\n+++\nruled")
}

/// A merge-main receipt for a merge that landed on main.
fn merged_main() -> String {
    push_receipt(
        0,
        "",
        &format!("runner publish: pushed refs/heads/main {MERGED} {MERGED}"),
        "",
    )
}

/// Routes a `slicer` role so the restarted run can start its slicer job.
fn with_slicer(rig: &mut Rig) {
    let mut routing = (*rig.ctx.routing).clone();
    routing.roles.push(RoutedRole {
        name: String::from("slicer"),
        kind: Some(String::from("slicer")),
        status: String::from("active"),
        pool: Some(String::from("test-planner-pool")),
        read_only: Some(true),
        model: None,
        models: Vec::new(),
    });
    rig.ctx = rig.ctx.clone().with_routing(routing);
    rig.registry = shipped(&rig.ctx);
}

/// Seeds run 1 with the completed admission a restart at `slice` consumes: a
/// planner step `planned` with its accepted `document`, and an admitter that
/// answered `admitted`.
fn seed_admission(rig: &Rig, document: &str) -> R {
    rig.store.insert_step(
        &rig.need,
        &StepRow {
            run: 1,
            node: String::from("plan"),
            task: String::from("t-plan"),
            state: StepState::Done,
            verdict: Some(String::from("planned")),
        },
    )?;
    rig.store.insert_step(
        &rig.need,
        &StepRow {
            run: 1,
            node: String::from("admit1"),
            task: String::from("t-admit"),
            state: StepState::Done,
            verdict: Some(String::from("admitted")),
        },
    )?;
    rig.store.record_step_payload(&StepPayloadRow {
        need: rig.need.clone(),
        run: 1,
        node: String::from("plan"),
        class: String::from("planner"),
        task: String::from("t-plan"),
        payload: serde_json::json!({ "body": document }),
    })?;
    Ok(())
}

fn alarms(rig: &Rig) -> usize {
    rig.exceptions().matches(LOCK_ALARM).count()
}

/// The id of the open correction job ending in `suffix`.
fn correction(rig: &Rig, suffix: &str) -> Option<String> {
    rig.jobs
        .jobs
        .values()
        .find(|job| job.id.ends_with(suffix) && !job.status.is_terminal())
        .map(|job| job.id.clone())
}

/// Answer the open cto job with `word` and tick until run r2 is live; assert
/// the restart left two run rows, r1 not live, r2 live at `entry`, and the
/// need open.
fn restarts_at(rig: &mut Rig, word: &str, entry: &str) -> R {
    to_cto(rig)?;
    finish(rig, "cto", &agent_receipt(0, &ruling(word)))?;
    rig.tick_until("run 2 live", |rig| {
        rig.runs()
            .is_ok_and(|runs| runs.len() == 2 && runs[1].state == "live")
    })?;
    let runs = rig.runs()?;
    assert_eq!(runs.len(), 2, "{word}");
    assert_eq!(runs[0].run, 1, "{word}");
    assert_ne!(runs[0].state, "live", "{word}: r1 is not live");
    assert_eq!(runs[1].run, 2, "{word}");
    assert_eq!(runs[1].state, "live", "{word}");
    assert_eq!(runs[1].entry, entry, "{word}");
    assert!(rig.row(entry)?.is_some(), "{word}: the entry step of r2");
    let cto = rig
        .store
        .steps_of(&rig.need, 1)?
        .into_iter()
        .find(|row| row.node == "cto")
        .map(|row| (row.state, row.verdict));
    assert_eq!(cto, Some((StepState::Done, Some(word.to_string()))));
    assert_ne!(rig.port.need_state(&rig.need)?, NeedStatus::Done, "{word}");
    assert_eq!(rig.exceptions(), "", "{word}");
    Ok(())
}

#[test]
fn a_cto_research_answer_restarts_the_need_at_research() -> R {
    let mut rig = build()?;
    restarts_at(&mut rig, "research", "research")
}

#[test]
fn a_cto_plan_answer_restarts_the_need_at_plan() -> R {
    let mut rig = build()?;
    restarts_at(&mut rig, "plan", "plan")
}

#[test]
fn a_cto_slice_answer_restarts_the_need_at_slice() -> R {
    let mut rig = build()?;
    with_slicer(&mut rig);
    // Run 1 carries a completed admission; the restart consumes it.
    seed_admission(&rig, "PLAN-ONE")?;
    restarts_at(&mut rig, "slice", "slice")?;
    // The restarted slicer consumes run 1's admission: its job freezes the
    // source identities and the accepted document, and run 2 opens no
    // planner of its own.
    rig.tick_until("the r2 slicer job", |rig| rig.open_job("slice").is_some())?;
    let job = rig
        .open_job("slice")
        .ok_or_else(|| VivariumError::Message("no slicer job for run 2".into()))?;
    assert!(
        job.body
            .contains("admitted plan source: run 1 planner task t-plan admitter task t-admit"),
        "{}",
        job.body
    );
    assert!(
        job.body.contains("admitted plan:\nPLAN-ONE"),
        "{}",
        job.body
    );
    assert_eq!(
        job.payload["admitted_plan"],
        serde_json::json!({ "run": 1, "planner": "t-plan", "admitter": "t-admit" })
    );
    assert!(rig.open_job("plan").is_none());
    assert!(rig.open_job("admit1").is_none());
    Ok(())
}

#[test]
fn a_cto_rework_answer_restarts_the_need_at_rework1() -> R {
    let mut rig = build()?;
    restarts_at(&mut rig, "rework", "rework1")
}

#[test]
fn a_cto_rework_restart_is_driven_to_done_and_the_need_closes_once() -> R {
    let mut rig = build()?;
    restarts_at(&mut rig, "rework", "rework1")?;
    finish(&mut rig, "rework1", &agent_receipt(0, SLICED))?;
    rig.tick_until("the audit2 and test2 jobs of r2", |rig| {
        rig.open_job("audit2").is_some() && rig.open_job("test2").is_some()
    })?;
    finish(&mut rig, "audit2", &agent_receipt(0, CLEAN))?;
    finish(&mut rig, "test2", &agent_receipt(0, "all tests passed"))?;
    finish(&mut rig, "merge", &merged_main())?;
    rig.tick_until("run 2 done", |rig| {
        rig.runs()
            .is_ok_and(|runs| runs.len() == 2 && runs[1].state == "done")
    })?;
    let runs = rig.runs()?;
    assert_ne!(runs[0].state, "live");
    assert_eq!(runs[1].state, "done", "{}", rig.exceptions());
    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Done);
    assert_eq!(
        rig.port.last_closed_record(&rig.need)?.as_deref(),
        Some("landed")
    );
    for _ in 0..3 {
        rig.tick()?;
    }
    assert_eq!(rig.runs()?.len(), 2);
    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Done);
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn a_failed_cto_locks_the_run_with_one_alarm() -> R {
    let mut rig = build()?;
    to_cto(&mut rig)?;
    // An envelope that never parses: two corrections, then `failed`.
    finish(&mut rig, "cto", &agent_receipt(0, "no envelope here"))?;
    for n in 1..=2 {
        let suffix = format!("-c{n}");
        rig.tick_until("the correction job", |rig| {
            correction(rig, &suffix).is_some()
        })?;
        let id = correction(&rig, &suffix)
            .ok_or_else(|| VivariumError::Message(format!("no correction job {suffix}")))?;
        let receipt = agent_receipt(0, "still no envelope");
        rig.finish(&id, JobStatus::Finished, &receipt)?;
    }
    rig.tick_until("locked", |rig| {
        rig.row("locked").is_ok_and(|row| row.is_some())
    })?;
    for _ in 0..3 {
        rig.tick()?;
    }
    let locked = rig.row("locked")?.map(|row| (row.state, row.verdict));
    assert_eq!(locked, Some((StepState::Waiting, None)));
    let cto = rig.row("cto")?.and_then(|row| row.verdict);
    assert_eq!(cto.as_deref(), Some("failed"));
    assert_eq!(rig.runs()?.len(), 1);
    assert_eq!(rig.runs()?[0].state, "live");
    assert_eq!(alarms(&rig), 1, "{}", rig.exceptions());
    Ok(())
}
