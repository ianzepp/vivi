//! End to end: the build flow from the merge on, over a real mailspace.
//!
//! The run starts at `rework1` (the only entry that skips the planning front)
//! and is driven through a clean second verify pass to the `merge` node; the
//! fanout is replaced by a handler that answers `built` at once. Every job the
//! pump enqueues is finished here with a scripted receipt, as the pool would,
//! and the tick is driven until the next step opens. Tests: merge and the sync
//! loop (`sync1`, `st1`, `merge2`), the routes to the CTO, the nothing-to-merge
//! answer, the env hold, the lock alarm and the per-step error hold.

use super::e2e_rig::{Rig, agent_receipt, env_receipt, push_receipt, rig};
use crate::error::VivariumError;
use crate::pump::config::{PumpConfig, RungsConfig};
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::handlers::shipped;
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::flow::mail::{MailReader, NeedStatus};
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::step_error::STEP_ERROR_HELD;
use crate::pump::flow::tick::{OPERATOR_HOLD, RUN_ERROR};
use crate::pump::jobs::JobStatus;
use crate::pump::jobs::rung_result::ENV_FAILURE;
use crate::pump::store::NeedBranchRow;
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
/// from `base`, with main at [`MAIN`] and a merge role routed.
fn build(base: &str) -> Result<Rig, VivariumError> {
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
        base: Some(base.into()),
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

/// Drive the run from `rework1` through a clean second pass to the `merge`
/// step's job.
fn to_merge(rig: &mut Rig) -> R {
    finish(rig, "rework1", &agent_receipt(0, SLICED))?;
    rig.tick_until("the audit2 and test2 jobs", |rig| {
        rig.open_job("audit2").is_some() && rig.open_job("test2").is_some()
    })?;
    finish(rig, "audit2", &agent_receipt(0, CLEAN))?;
    finish(rig, "test2", &agent_receipt(0, "all tests passed"))?;
    open(rig, "merge").map(drop)
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

/// A merge-main receipt for a real merge conflict.
fn conflict_main() -> String {
    agent_receipt(1, "CONFLICT (content): merge conflict in a.rs")
}

/// A sync receipt: the merge role published the need branch.
fn synced(rig: &Rig) -> String {
    let publish = format!(
        "runner publish: pushed refs/heads/vivi/need/{} {MERGED} {MERGED}",
        rig.need
    );
    push_receipt(0, "", &publish, "+++\nanswer = \"merged\"\n+++\nsynced")
}

/// A sync receipt for a merge that stopped on conflicts: no publish.
fn sync_conflict() -> String {
    agent_receipt(
        0,
        "+++\nanswer = \"conflict\"\nconflicts = [\"a.rs\"]\n+++\nstuck",
    )
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

fn verdict(rig: &Rig, node: &str) -> Result<Option<String>, VivariumError> {
    Ok(rig.row(node)?.and_then(|row| row.verdict))
}

fn run_done(rig: &Rig) -> bool {
    rig.runs()
        .is_ok_and(|runs| runs.iter().any(|run| run.state == "done"))
}

fn assert_landed(rig: &Rig) -> R {
    assert_eq!(rig.runs()?[0].state, "done", "{}", rig.exceptions());
    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Done);
    assert_eq!(
        rig.port.last_closed_record(&rig.need)?.as_deref(),
        Some("landed")
    );
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

/// A conflicting merge, a published sync and a passing `st1`, up to the
/// `merge2` job.
fn to_merge2(rig: &mut Rig) -> R {
    to_merge(rig)?;
    finish(rig, "merge", &conflict_main())?;
    let receipt = synced(rig);
    finish(rig, "sync1", &receipt)?;
    finish(rig, "st1", &agent_receipt(0, "all tests passed"))?;
    open(rig, "merge2").map(drop)
}

#[test]
fn a_merged_receipt_lands_the_need() -> R {
    let mut rig = build(BASE)?;
    to_merge(&mut rig)?;
    finish(&mut rig, "merge", &merged_main())?;
    rig.tick_until("run done", run_done)?;
    assert_eq!(verdict(&rig, "merge")?.as_deref(), Some("merged"));
    assert_landed(&rig)
}

#[test]
fn a_conflict_syncs_tests_and_merges_again() -> R {
    let mut rig = build(BASE)?;
    to_merge(&mut rig)?;
    finish(&mut rig, "merge", &conflict_main())?;
    let sync = open(&mut rig, "sync1")?;
    assert_eq!(verdict(&rig, "merge")?.as_deref(), Some("conflict"));
    let receipt = synced(&rig);
    rig.finish(&sync, JobStatus::Finished, &receipt)?;
    finish(&mut rig, "st1", &agent_receipt(0, "all tests passed"))?;
    assert_eq!(verdict(&rig, "sync1")?.as_deref(), Some("merged"));
    open(&mut rig, "merge2")?;
    assert_eq!(verdict(&rig, "st1")?.as_deref(), Some("pass"));
    finish(&mut rig, "merge2", &merged_main())?;
    rig.tick_until("run done", run_done)?;
    assert_eq!(verdict(&rig, "merge2")?.as_deref(), Some("merged"));
    assert_landed(&rig)
}

#[test]
fn a_failing_st1_goes_to_the_cto() -> R {
    let mut rig = build(BASE)?;
    to_merge(&mut rig)?;
    finish(&mut rig, "merge", &conflict_main())?;
    let receipt = synced(&rig);
    finish(&mut rig, "sync1", &receipt)?;
    finish(&mut rig, "st1", &agent_receipt(1, "test x ... FAILED"))?;
    open(&mut rig, "cto")?;
    assert_eq!(verdict(&rig, "st1")?.as_deref(), Some("fail"));
    assert!(rig.row("merge2")?.is_none());
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn a_conflict_at_merge2_goes_to_the_cto() -> R {
    let mut rig = build(BASE)?;
    to_merge2(&mut rig)?;
    finish(&mut rig, "merge2", &conflict_main())?;
    open(&mut rig, "cto")?;
    assert_eq!(verdict(&rig, "merge2")?.as_deref(), Some("conflict"));
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn a_sync_that_publishes_nothing_goes_to_the_cto() -> R {
    let mut rig = build(BASE)?;
    to_merge(&mut rig)?;
    finish(&mut rig, "merge", &conflict_main())?;
    finish(&mut rig, "sync1", &sync_conflict())?;
    open(&mut rig, "cto")?;
    assert_eq!(verdict(&rig, "sync1")?.as_deref(), Some("conflict"));
    assert!(rig.row("st1")?.is_none());
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn a_need_tip_equal_to_the_run_base_fails_the_merge_and_reaches_the_cto() -> R {
    let mut rig = build(TIP)?;
    finish(&mut rig, "rework1", &agent_receipt(0, SLICED))?;
    rig.tick_until("the audit2 and test2 jobs", |rig| {
        rig.open_job("audit2").is_some() && rig.open_job("test2").is_some()
    })?;
    finish(&mut rig, "audit2", &agent_receipt(0, CLEAN))?;
    finish(&mut rig, "test2", &agent_receipt(0, "all tests passed"))?;
    open(&mut rig, "cto")?;
    assert_eq!(verdict(&rig, "merge")?.as_deref(), Some("failed"));
    assert!(rig.open_job("merge").is_none());
    let task = rig
        .row("merge")?
        .map(|row| row.task)
        .ok_or_else(|| VivariumError::Message("no merge row".into()))?;
    let stored = rig
        .store
        .step_payload(&task)?
        .ok_or_else(|| VivariumError::Message("no merge payload".into()))?;
    let reason = stored.payload["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("nothing to merge") && reason.contains(TIP),
        "{reason}"
    );
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn an_st1_with_a_missing_tool_is_held_with_one_env_alarm_and_no_cto() -> R {
    let mut rig = build(BASE)?;
    to_merge(&mut rig)?;
    finish(&mut rig, "merge", &conflict_main())?;
    let receipt = synced(&rig);
    finish(&mut rig, "sync1", &receipt)?;
    finish(&mut rig, "st1", &env_receipt("cargo"))?;
    rig.tick_until("st1 held", |rig| {
        rig.row("st1")
            .is_ok_and(|row| row.is_some_and(|row| row.verdict.as_deref() == Some(OPERATOR_HOLD)))
    })?;
    for _ in 0..3 {
        rig.tick()?;
    }
    let held = rig.row("st1")?.map(|row| (row.state, row.verdict));
    assert_eq!(
        held,
        Some((StepState::Waiting, Some(OPERATOR_HOLD.to_string())))
    );
    assert_eq!(kinds(&rig), [ENV_FAILURE], "{}", rig.exceptions());
    assert!(rig.row("cto")?.is_none());
    assert_eq!(rig.runs()?.len(), 1);
    Ok(())
}

#[test]
fn a_cto_lock_locks_the_need_with_one_alarm() -> R {
    let mut rig = build(BASE)?;
    to_merge2(&mut rig)?;
    finish(&mut rig, "merge2", &conflict_main())?;
    let envelope = "+++\nanswer = \"lock\"\nreason = 'needs the operator'\n+++\nlocked";
    finish(&mut rig, "cto", &agent_receipt(0, envelope))?;
    rig.tick_until("locked", |rig| {
        rig.row("locked").is_ok_and(|row| row.is_some())
    })?;
    for _ in 0..3 {
        rig.tick()?;
    }
    let locked = rig.row("locked")?.map(|row| (row.state, row.verdict));
    assert_eq!(locked, Some((StepState::Waiting, None)));
    assert_eq!(rig.runs()?[0].state, "live");
    assert_eq!(kinds(&rig), [LOCK_ALARM], "{}", rig.exceptions());
    Ok(())
}

/// A start handler that always fails.
struct Broken;

impl StepHandler for Broken {
    fn start(&self, _cx: &StartCx<'_>, _step: &StepRow) -> Result<StartAction, VivariumError> {
        Err(VivariumError::Message("handler broke".into()))
    }
}

#[test]
fn a_handler_that_fails_three_ticks_running_holds_the_step_with_one_alarm() -> R {
    let mut rig = build(BASE)?;
    rig.registry
        .node_handler(FlowKind::Build, "rework1", Box::new(Broken));
    for _ in 0..2 {
        rig.tick()?;
        assert_eq!(rig.exceptions().matches(STEP_ERROR_HELD).count(), 0);
    }
    rig.tick()?;
    rig.tick()?;
    let held = rig.row("rework1")?.map(|row| (row.state, row.verdict));
    assert_eq!(
        held,
        Some((StepState::Waiting, Some(OPERATOR_HOLD.to_string())))
    );
    // The failing tick's run boundary raises its own alarm, once per distinct
    // error, beside the hold.
    assert_eq!(
        kinds(&rig),
        [RUN_ERROR, STEP_ERROR_HELD],
        "{}",
        rig.exceptions()
    );
    assert!(rig.exceptions().contains("handler broke"));
    Ok(())
}

#[test]
fn a_sync1_whose_agent_cannot_run_is_held_with_one_env_alarm_and_no_cto() -> R {
    let mut rig = build(BASE)?;
    to_merge(&mut rig)?;
    finish(&mut rig, "merge", &conflict_main())?;
    finish(&mut rig, "sync1", &env_receipt("claude"))?;
    rig.tick_until("sync1 held", |rig| {
        rig.row("sync1")
            .is_ok_and(|row| row.is_some_and(|row| row.verdict.as_deref() == Some(OPERATOR_HOLD)))
    })?;
    for _ in 0..3 {
        rig.tick()?;
    }
    let held = rig.row("sync1")?.map(|row| (row.state, row.verdict));
    assert_eq!(
        held,
        Some((StepState::Waiting, Some(OPERATOR_HOLD.to_string())))
    );
    assert_eq!(kinds(&rig), [ENV_FAILURE], "{}", rig.exceptions());
    assert!(rig.row("cto")?.is_none());
    assert!(rig.row("st1")?.is_none());
    Ok(())
}
