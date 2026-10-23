//! End to end gate: two needs on one mailspace.
//!
//! Region: the tick across needs. Covers runs of different needs that share a
//! target and a pool, so one need's steps and jobs do not touch the other's.
//! W2-IN-7 (c), as ruled: a repair merge-main never waits behind QUEUED
//! ordinary work (`rank::flow_key` puts repair first), but one merge-main per
//! target in flight is a correctness invariant (`rank::single_flight`), so a
//! repair cannot preempt an in-flight merge-main: it goes next, ahead of any
//! newer ordinary one, and the ruled wait raises no alarm.

use super::e2e_rig::{Rig, agent_receipt, push_receipt, rig};
use crate::error::VivariumError;
use crate::pump::config::{PumpConfig, RungsConfig};
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::handlers::shipped;
use crate::pump::flow::index::StepRow;
use crate::pump::flow::mail::{MailReader, NeedStatus};
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::jobs::{self, JobStatus, PoolJob};
use crate::pump::store::NeedBranchRow;
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

const BASE: &str = "1111111111111111111111111111111111111111";
const TIP: &str = "2222222222222222222222222222222222222222";
const MAIN: &str = "3333333333333333333333333333333333333333";
const MERGED: &str = "4444444444444444444444444444444444444444";
const CAND1: &str = "5555555555555555555555555555555555555555";
const TESTED: &str = "0000000000000000000000000000000000000000";
const SLICED: &str = "+++\nanswer = \"sliced\"\nmode = \"rework\"\n\n[[slice]]\nid = \"fix-1\"\n\
                      title = 'Fix it'\nbrief = 'fix the finding'\ntarget = \"default\"\n\
                      write_scope = [\"src/a.rs\"]\n+++\ncut one rework slice";
const CLEAN: &str = "+++\nanswer = \"clean_pass\"\n+++\nclean";
const BUILT: &str = "+++\nanswer = \"built\"\n+++\nfixed";

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

fn open_job(rig: &Rig, node: &str) -> Result<PoolJob, VivariumError> {
    rig.open_job(node)
        .ok_or_else(|| VivariumError::Message(format!("no open job for {node}")))
}

/// Tick until `node` has an open job, then finish it with `receipt`.
fn finish(rig: &mut Rig, node: &str, receipt: &str) -> R {
    rig.tick_until(&format!("a {node} job"), |rig| rig.open_job(node).is_some())?;
    let id = open_job(rig, node)?.id;
    rig.finish(&id, JobStatus::Finished, receipt)
}

/// An ordinary build need driven to a pending `merge` (merge-main) job, with
/// merge and auditor roles routed, main at [`MAIN`] and two free pool slots.
fn ordinary_at_merge() -> Result<Rig, VivariumError> {
    let mut rig = rig(config(), "kind: build", "rework1")?;
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
    rig.git(&format!("refs/heads/vivi/need/{}", rig.need), TIP);
    rig.git("main", MAIN);
    let shown = serde_json::json!([
        {"handle": rig.need, "message_id": "m1", "from": "operator@local",
         "subject": "Need", "body": "Build the thing"}
    ]);
    rig.project.host.reply("need show", 0, &shown.to_string());
    rig.project.host.reply(
        "pool status --porcelain",
        0,
        "slot test-planner-pool bridge slot-1 free\nslot test-planner-pool bridge slot-2 free",
    );
    rig.project
        .host
        .reply("pool dispatch", 0, "dispatched to test-planner-pool");
    finish(&mut rig, "rework1", &agent_receipt(0, SLICED))?;
    rig.tick_until("the audit2 and test2 jobs", |rig| {
        rig.open_job("audit2").is_some() && rig.open_job("test2").is_some()
    })?;
    finish(&mut rig, "audit2", &agent_receipt(0, CLEAN))?;
    finish(&mut rig, "test2", &agent_receipt(0, "all tests passed"))?;
    rig.tick_until("the merge job", |rig| rig.open_job("merge").is_some())?;
    Ok(rig)
}

/// A repair need added to the rig and driven to its pending `merge1` job:
/// fix1 built, main rung passed. Returns the repair need's handle.
fn repair_at_merge1(rig: &mut Rig) -> Result<String, VivariumError> {
    let record = format!("kind: repair\ntarget: default\ntested: {TESTED}\n\nFAILED a::b\n");
    let need = rig.add_need("a repair need beside a build need", &record, "fix1")?;
    rig.tick_until("a fix1 job", |rig| rig.open_job("fix1").is_some())?;
    let fix = open_job(rig, "fix1")?;
    let reference = fix
        .publish_ref
        .clone()
        .ok_or_else(|| VivariumError::Message("fix job has no publish ref".into()))?;
    let publish = format!("runner publish: pushed {reference} {CAND1} {CAND1}");
    let receipt = push_receipt(0, "pushed", &publish, BUILT);
    rig.finish(&fix.id, JobStatus::Finished, &receipt)?;
    finish(rig, "rung1", &agent_receipt(0, ""))?;
    rig.tick_until("the merge1 job", |rig| rig.open_job("merge1").is_some())?;
    Ok(need)
}

fn status_of(rig: &Rig, id: &str) -> Result<JobStatus, VivariumError> {
    rig.jobs
        .jobs
        .get(id)
        .map(|job| job.status)
        .ok_or_else(|| VivariumError::Message(format!("no job {id}")))
}

/// Each `task send` of the dispatcher creates a fresh task handle.
fn script_task_handles(rig: &Rig) {
    for handle in ["aaaaaaa1", "aaaaaaa2", "aaaaaaa3"] {
        rig.project
            .host
            .reply_once("task send", 0, &format!("created {handle}"));
    }
}

fn run_state(rig: &Rig, need: &str) -> Result<String, VivariumError> {
    Ok(rig
        .store
        .runs_of(need)?
        .last()
        .map(|run| run.state.clone())
        .unwrap_or_default())
}

#[test]
fn a_repair_merge_main_is_dispatched_before_an_older_ordinary_merge_main() -> R {
    let mut rig = ordinary_at_merge()?;
    let ordinary = open_job(&rig, "merge")?;
    let repair_need = repair_at_merge1(&mut rig)?;
    let repair = open_job(&rig, "merge1")?;
    assert!(
        ordinary.queued_at <= repair.queued_at,
        "the ordinary merge was queued first"
    );
    script_task_handles(&rig);
    let started = jobs::dispatch_pending(&rig.ctx, &mut rig.jobs)?;
    assert!(started.contains(&repair.id), "{started:?}");
    assert!(!started.contains(&ordinary.id), "{started:?}");
    assert_eq!(status_of(&rig, &repair.id)?, JobStatus::Dispatched);
    assert_eq!(status_of(&rig, &ordinary.id)?, JobStatus::Pending);

    // The repair lands while the ordinary merge still waits.
    let publish = format!("runner publish: pushed refs/heads/main {MERGED} {MERGED}");
    rig.finish(
        &repair.id,
        JobStatus::Finished,
        &push_receipt(0, "", &publish, ""),
    )?;
    rig.tick_until("the repair run done", |rig| {
        run_state(rig, &repair_need).is_ok_and(|state| state == "done")
    })?;
    assert_eq!(rig.port.need_state(&repair_need)?, NeedStatus::Done);
    assert_eq!(status_of(&rig, &ordinary.id)?, JobStatus::Pending);
    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Open);
    assert_ne!(run_state(&rig, &rig.need)?, "done");
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

/// A second ordinary build need brought to a pending merge-main job, queued
/// after whatever the rig already holds. Returns that job.
fn second_ordinary_at_merge(rig: &mut Rig) -> Result<PoolJob, VivariumError> {
    let known: Vec<String> = rig.jobs.jobs.keys().cloned().collect();
    let need = rig.add_need("a second build need", "kind: build", "rework1")?;
    rig.store.upsert_need_branch(&NeedBranchRow {
        need: need.clone(),
        branch: format!("vivi/need/{need}"),
        base: Some(BASE.into()),
        status: "live".into(),
        run: 1,
        target: "default".into(),
        priors: Vec::new(),
    })?;
    rig.git(&format!("refs/heads/vivi/need/{need}"), TIP);
    let shown = serde_json::json!([
        {"handle": rig.need, "message_id": "m1", "from": "operator@local",
         "subject": "Need", "body": "Build the thing"},
        {"handle": need, "message_id": "m2", "from": "operator@local",
         "subject": "Need", "body": "Build another thing"}
    ]);
    rig.project.host.reply("need show", 0, &shown.to_string());
    finish(rig, "rework1", &agent_receipt(0, SLICED))?;
    rig.tick_until("the second audit2 and test2 jobs", |rig| {
        rig.open_job("audit2").is_some() && rig.open_job("test2").is_some()
    })?;
    finish(rig, "audit2", &agent_receipt(0, CLEAN))?;
    finish(rig, "test2", &agent_receipt(0, "all tests passed"))?;
    let fresh = |rig: &Rig| {
        rig.jobs
            .jobs
            .values()
            .find(|job| {
                !known.contains(&job.id) && job.payload.get("node").is_some_and(|n| n == "merge")
            })
            .cloned()
    };
    rig.tick_until("the second ordinary merge job", |rig| fresh(rig).is_some())?;
    fresh(rig).ok_or_else(|| VivariumError::Message("no second ordinary merge job".into()))
}

/// W2-IN-7 ruling: one merge-main in flight per target is a correctness
/// invariant (two concurrent merges to one main would conflict under the CAS
/// rule). A repair never waits behind queued ordinary work, but it cannot
/// preempt an in-flight job: it goes next. While the ordinary merge-main is
/// dispatched the repair stays pending without an alarm, a newer ordinary
/// merge-main queued meanwhile does not overtake it, and when the in-flight
/// job ends the repair is dispatched and the newer ordinary one still waits.
#[test]
fn a_repair_merge_main_waits_only_for_the_ordinary_merge_in_flight() -> R {
    let mut rig = ordinary_at_merge()?;
    let ordinary = open_job(&rig, "merge")?;
    script_task_handles(&rig);
    jobs::dispatch_pending(&rig.ctx, &mut rig.jobs)?;
    assert_eq!(status_of(&rig, &ordinary.id)?, JobStatus::Dispatched);
    repair_at_merge1(&mut rig)?;
    let repair = open_job(&rig, "merge1")?;

    // The repair does not preempt the in-flight merge; the wait is ruled, so
    // it raises nothing.
    jobs::dispatch_pending(&rig.ctx, &mut rig.jobs)?;
    for _ in 0..3 {
        rig.tick()?;
        jobs::dispatch_pending(&rig.ctx, &mut rig.jobs)?;
    }
    assert_eq!(status_of(&rig, &repair.id)?, JobStatus::Pending);
    assert_eq!(status_of(&rig, &ordinary.id)?, JobStatus::Dispatched);
    assert_eq!(rig.exceptions(), "");

    // A newer ordinary merge-main queued after the repair.
    let newer = second_ordinary_at_merge(&mut rig)?;
    assert!(
        repair.queued_at <= newer.queued_at,
        "the newer one was queued last"
    );
    jobs::dispatch_pending(&rig.ctx, &mut rig.jobs)?;
    assert_eq!(status_of(&rig, &repair.id)?, JobStatus::Pending);
    assert_eq!(status_of(&rig, &newer.id)?, JobStatus::Pending);
    assert_eq!(rig.exceptions(), "");

    // The moment the ordinary merge ends, the repair merge goes next.
    let publish = format!("runner publish: pushed refs/heads/main {MERGED} {MERGED}");
    rig.finish(
        &ordinary.id,
        JobStatus::Finished,
        &push_receipt(0, "", &publish, ""),
    )?;
    script_task_handles(&rig);
    jobs::dispatch_pending(&rig.ctx, &mut rig.jobs)?;
    assert_eq!(status_of(&rig, &repair.id)?, JobStatus::Dispatched);
    assert_eq!(status_of(&rig, &newer.id)?, JobStatus::Pending);
    Ok(())
}
