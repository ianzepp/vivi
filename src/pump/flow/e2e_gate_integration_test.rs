//! End to end gate: the merge, sync and second merge edges.
//!
//! Region: `merge`, `sync1`, `st1` and `merge2`. Edges covered beyond
//! `e2e_build_test`: merge `failed`; sync1 `conflict` and `failed`; st1 lost
//! once and retried, and st1 `failed`; merge2 `failed`.
//!
//! The run starts at `rework1` over a real mailspace, as `e2e_build_test`
//! does; the helpers below are local copies of its setup.

use super::e2e_rig::{Rig, agent_receipt, push_receipt, rig};
use crate::error::VivariumError;
use crate::pump::config::{PumpConfig, RungsConfig};
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::handlers::shipped;
use crate::pump::flow::index::StepRow;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::jobs::JobStatus;
use crate::pump::store::NeedBranchRow;
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

/// The main tip the need branch was cut from.
const BASE: &str = "1111111111111111111111111111111111111111";
/// The need branch tip: ahead of [`BASE`].
const TIP: &str = "2222222222222222222222222222222222222222";
/// The current tip of main.
const MAIN: &str = "3333333333333333333333333333333333333333";
/// The commit a publish reports.
const MERGED: &str = "4444444444444444444444444444444444444444";

/// Times the pump queues a lost merge again before it answers `failed`.
const MOVED_RETRIES: u32 = 3;

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

/// Tick until `node` has an open job, then finish it with `receipt`.
fn finish(rig: &mut Rig, node: &str, receipt: &str) -> R {
    let id = open(rig, node)?;
    rig.finish(&id, JobStatus::Finished, receipt)
}

/// Tick until `node` has an open job, then end it as the pool would a lost
/// slot.
fn lose(rig: &mut Rig, node: &str) -> R {
    let id = open(rig, node)?;
    rig.finish(&id, JobStatus::Lost, "")
}

/// Lose the open `node` job and tick until its one retry is open.
fn lose_once(rig: &mut Rig, node: &str) -> R {
    let first = open(rig, node)?;
    rig.finish(&first, JobStatus::Lost, "")?;
    rig.tick_until(&format!("the {node} retry"), |rig| {
        rig.open_job(node).is_some_and(|job| job.id != first)
    })
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

/// A merge-main receipt for a real merge conflict.
fn conflict_main() -> String {
    agent_receipt(1, "CONFLICT (content): merge conflict in a.rs")
}

/// A merge-main receipt for a failure that is not a conflict.
fn broken_main() -> String {
    agent_receipt(1, "fatal: couldn't find remote ref")
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

/// A sync receipt for a worker that crashed: no publish, a failing exit.
fn sync_broken() -> String {
    agent_receipt(1, "the worker crashed")
}

fn verdict(rig: &Rig, node: &str) -> Result<Option<String>, VivariumError> {
    Ok(rig.row(node)?.and_then(|row| row.verdict))
}

/// A conflicting merge, then a published sync, up to the `st1` job.
fn to_st1(rig: &mut Rig) -> R {
    to_merge(rig)?;
    finish(rig, "merge", &conflict_main())?;
    let receipt = synced(rig);
    finish(rig, "sync1", &receipt)?;
    open(rig, "st1").map(drop)
}

/// A passing `st1`, up to the `merge2` job.
fn to_merge2(rig: &mut Rig) -> R {
    to_st1(rig)?;
    finish(rig, "st1", &agent_receipt(0, "all tests passed"))?;
    open(rig, "merge2").map(drop)
}

/// The run went to the CTO with `node` answering `word`, the steps in
/// `later` never started, and no alarm was raised.
fn assert_cto_after(rig: &mut Rig, node: &str, word: &str, later: &[&str]) -> R {
    open(rig, "cto")?;
    assert_eq!(verdict(rig, node)?.as_deref(), Some(word));
    for step in later {
        assert!(rig.row(step)?.is_none(), "{step} must not have started");
    }
    assert_eq!(rig.exceptions(), "");
    assert_eq!(rig.runs()?.len(), 1);
    Ok(())
}

#[test]
fn a_merge_that_fails_goes_to_the_cto() -> R {
    let mut rig = build()?;
    to_merge(&mut rig)?;
    finish(&mut rig, "merge", &broken_main())?;
    assert_cto_after(&mut rig, "merge", "failed", &["sync1", "merge2"])
}

#[test]
fn a_merge_job_lost_on_every_attempt_with_main_unmoved_goes_to_the_cto() -> R {
    let mut rig = build()?;
    to_merge(&mut rig)?;
    for _ in 0..MOVED_RETRIES {
        lose_once(&mut rig, "merge")?;
    }
    lose(&mut rig, "merge")?;
    assert_cto_after(&mut rig, "merge", "failed", &["sync1", "merge2"])
}

#[test]
fn a_sync_that_conflicts_goes_to_the_cto() -> R {
    let mut rig = build()?;
    to_merge(&mut rig)?;
    finish(&mut rig, "merge", &conflict_main())?;
    finish(&mut rig, "sync1", &sync_conflict())?;
    assert_cto_after(&mut rig, "sync1", "conflict", &["st1", "merge2"])?;
    assert_eq!(verdict(&rig, "merge")?.as_deref(), Some("conflict"));
    Ok(())
}

#[test]
fn a_sync_that_fails_goes_to_the_cto() -> R {
    let mut rig = build()?;
    to_merge(&mut rig)?;
    finish(&mut rig, "merge", &conflict_main())?;
    finish(&mut rig, "sync1", &sync_broken())?;
    assert_cto_after(&mut rig, "sync1", "failed", &["st1", "merge2"])
}

#[test]
fn a_lost_sync_job_with_the_need_branch_unmoved_goes_to_the_cto() -> R {
    let mut rig = build()?;
    to_merge(&mut rig)?;
    finish(&mut rig, "merge", &conflict_main())?;
    lose(&mut rig, "sync1")?;
    assert_cto_after(&mut rig, "sync1", "failed", &["st1", "merge2"])
}

#[test]
fn an_st1_lost_once_is_retried_and_its_pass_reaches_merge2() -> R {
    let mut rig = build()?;
    to_st1(&mut rig)?;
    lose_once(&mut rig, "st1")?;
    assert!(rig.row("merge2")?.is_none());
    finish(&mut rig, "st1", &agent_receipt(0, "all tests passed"))?;
    open(&mut rig, "merge2")?;
    assert_eq!(verdict(&rig, "st1")?.as_deref(), Some("pass"));
    assert!(rig.row("cto")?.is_none());
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn an_st1_lost_once_and_failing_on_the_retry_goes_to_the_cto() -> R {
    let mut rig = build()?;
    to_st1(&mut rig)?;
    lose_once(&mut rig, "st1")?;
    finish(&mut rig, "st1", &agent_receipt(1, "test x ... FAILED"))?;
    assert_cto_after(&mut rig, "st1", "fail", &["merge2"])
}

#[test]
fn an_st1_lost_twice_answers_failed_and_goes_to_the_cto() -> R {
    let mut rig = build()?;
    to_st1(&mut rig)?;
    lose_once(&mut rig, "st1")?;
    lose(&mut rig, "st1")?;
    assert_cto_after(&mut rig, "st1", "failed", &["merge2"])
}

#[test]
fn a_merge2_that_fails_goes_to_the_cto() -> R {
    let mut rig = build()?;
    to_merge2(&mut rig)?;
    finish(&mut rig, "merge2", &broken_main())?;
    assert_cto_after(&mut rig, "merge2", "failed", &[])
}

#[test]
fn a_merge2_job_lost_on_every_attempt_with_main_unmoved_goes_to_the_cto() -> R {
    let mut rig = build()?;
    to_merge2(&mut rig)?;
    for _ in 0..MOVED_RETRIES {
        lose_once(&mut rig, "merge2")?;
    }
    lose(&mut rig, "merge2")?;
    assert_cto_after(&mut rig, "merge2", "failed", &[])
}
