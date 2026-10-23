//! End to end gate: the rework and verify pass 2 edges.
//!
//! Region: `rework1`, `build2`, `audit2`, `test2`, `join2`, `judge2`, the raise
//! check and `raise2`. Edges covered: rework1 `sliced`, `escalate`, `failed`;
//! build2 `built`, `failed`, `unchanged`; join2 `verified`, `judge`, `block`,
//! `failed`, `raised`; judge2 and raise2 outcomes; a full rework pass to done.
//!
//! Every test runs a build need on a real mailspace through `tick_with` with
//! the shipped registry. The fanout is replaced by a handler that answers at
//! once; every other job is finished here with a scripted receipt, as the pool
//! would, and the tick is driven until the next step opens.

use super::e2e_rig::{Rig, agent_receipt, push_receipt, rig};
use crate::error::VivariumError;
use crate::pump::config::{PumpConfig, RungsConfig};
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::handlers::shipped;
use crate::pump::flow::index::StepRow;
use crate::pump::flow::mail::{MailReader, NeedStatus};
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
/// The commit a merge publishes.
const MERGED: &str = "4444444444444444444444444444444444444444";

const SLICED: &str = "+++\nanswer = \"sliced\"\n\n[[slice]]\nid = \"a\"\ntitle = 'Build it'\n\
                      brief = 'build the thing'\ntarget = \"default\"\n\
                      write_scope = [\"src/a.rs\"]\n+++\ncut one slice";
const REWORK_SLICED: &str = "+++\nanswer = \"sliced\"\nmode = \"rework\"\n\n[[slice]]\nid = \"fix-1\"\n\
                             title = 'Fix it'\nbrief = 'fix the finding'\ntarget = \"default\"\n\
                             write_scope = [\"src/a.rs\"]\n+++\ncut one rework slice";
const REWORK_ESCALATE: &str =
    "+++\nanswer = \"escalate\"\nmode = \"rework\"\nreason = 'cannot cut it'\n+++\nescalate";
const PLANNED: &str = "+++\nanswer = \"planned\"\n+++\nplan";
const ADMITTED: &str = "+++\nanswer = \"admitted\"\n+++\nadmitted";
const CLEAN: &str = "+++\nanswer = \"clean_pass\"\n+++\nclean";
const BLOCK: &str = "+++\nanswer = \"block_range\"\n+++\nblocked";
const RAISES_MAIN: &str = "+++\nanswer = \"residual\"\nraise_rung = \"main\"\n+++\nrun main";
const JUDGE_RESIDUAL: &str =
    "+++\nanswer = \"residual\"\nreason = 'minor leftovers'\n+++\nresidual";
const JUDGE_BLOCK: &str = "+++\nanswer = \"block\"\nreason = 'the failure is real'\n+++\nblock";

fn passing() -> String {
    agent_receipt(0, "all tests passed")
}

fn failing() -> String {
    agent_receipt(1, "test x ... FAILED")
}

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

/// Stands in for the fanout handler: every fanout step answers `word` at once.
struct Fanout(&'static str);

impl StepHandler for Fanout {
    fn start(&self, _cx: &StartCx<'_>, _step: &StepRow) -> Result<StartAction, VivariumError> {
        Ok(StartAction::Decide {
            word: self.0.into(),
        })
    }
}

/// A build run started at `entry` over a live need branch at [`TIP`] cut from
/// [`BASE`], with main at [`MAIN`] and a merge role routed.
fn build(entry: &str) -> Result<Rig, VivariumError> {
    let mut rig = rig(config(), "kind: build", entry)?;
    let mut routing = (*rig.ctx.routing).clone();
    for (name, read_only) in [("merge", false), ("admitter", true), ("slicer", true)] {
        routing.roles.push(RoutedRole {
            name: String::from(name),
            kind: Some(String::from(name)),
            status: String::from("active"),
            pool: Some(String::from("test-planner-pool")),
            read_only: Some(read_only),
            model: None,
            models: Vec::new(),
        });
    }
    rig.ctx = rig.ctx.with_routing(routing);
    rig.registry = shipped(&rig.ctx);
    rig.registry.handler(
        FlowKind::Build,
        NodeClass::Fanout,
        Box::new(Fanout("built")),
    );
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

/// Finish `node`'s job with `receipt` and record it on the step task, as the
/// pool does, where a later step (`admit1`, `slice`) reads its predecessor.
fn finish_noted(rig: &mut Rig, node: &str, receipt: &str) -> R {
    let id = open(rig, node)?;
    let task = rig
        .store
        .step_job(&id)?
        .map(|row| row.task)
        .ok_or_else(|| VivariumError::Message(format!("job {id} has no step row")))?;
    rig.finish(&id, JobStatus::Finished, receipt)?;
    rig.port.mailspace.record_pool_note(
        &task,
        "pool_collected",
        &format!("{{\"job\":\"{id}\"}}"),
        receipt,
    )?;
    Ok(())
}

/// Lose the first two attempts of `node`: the step fails after its retry.
fn lose_twice(rig: &mut Rig, node: &str) -> R {
    for _ in 0..2 {
        let id = open(rig, node)?;
        rig.finish(&id, JobStatus::Lost, "")?;
    }
    Ok(())
}

fn verdict(rig: &Rig, node: &str) -> Result<Option<String>, VivariumError> {
    Ok(rig.row(node)?.and_then(|row| row.verdict))
}

fn run_done(rig: &Rig) -> bool {
    rig.runs()
        .is_ok_and(|runs| runs.iter().any(|run| run.state == "done"))
}

/// Run `rework1` to `sliced` and wait for the `audit2` and `test2` jobs.
fn to_pass2(rig: &mut Rig) -> R {
    finish(rig, "rework1", &agent_receipt(0, REWORK_SLICED))?;
    rig.tick_until("the audit2 and test2 jobs", |rig| {
        rig.open_job("audit2").is_some() && rig.open_job("test2").is_some()
    })
}

/// A build run at `rework1`, through `build2`, with `audit` and `test`
/// answered: the join (or its follow-up) is the next thing to tick.
fn pass2(audit: &str, test: &str) -> Result<Rig, VivariumError> {
    let mut rig = build("rework1")?;
    to_pass2(&mut rig)?;
    finish(&mut rig, "audit2", &agent_receipt(0, audit))?;
    finish(&mut rig, "test2", test)?;
    Ok(rig)
}

/// The run is still the first one and still live, with no alarm raised.
fn assert_live_quiet(rig: &Rig) -> R {
    assert_eq!(rig.runs()?.len(), 1);
    assert_eq!(rig.runs()?[0].state, "live", "{}", rig.exceptions());
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

/// The CTO job opened; `node` answered `word`.
fn assert_cto(rig: &mut Rig, node: &str, word: &str) -> R {
    open(rig, "cto")?;
    assert_eq!(verdict(rig, node)?.as_deref(), Some(word));
    assert_live_quiet(rig)
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

#[test]
fn a_sliced_rework_opens_build2_and_the_second_verify_pass() -> R {
    let mut rig = build("rework1")?;
    finish(&mut rig, "rework1", &agent_receipt(0, REWORK_SLICED))?;
    open(&mut rig, "audit2")?;
    open(&mut rig, "test2")?;
    assert_eq!(verdict(&rig, "rework1")?.as_deref(), Some("sliced"));
    assert_eq!(verdict(&rig, "build2")?.as_deref(), Some("built"));
    assert_live_quiet(&rig)
}

#[test]
fn an_escalated_rework_goes_to_the_cto() -> R {
    let mut rig = build("rework1")?;
    finish(&mut rig, "rework1", &agent_receipt(0, REWORK_ESCALATE))?;
    assert_cto(&mut rig, "rework1", "escalate")?;
    assert!(rig.row("build2")?.is_none());
    Ok(())
}

#[test]
fn a_failed_rework_step_goes_to_the_cto() -> R {
    let mut rig = build("rework1")?;
    lose_twice(&mut rig, "rework1")?;
    assert_cto(&mut rig, "rework1", "failed")?;
    assert!(rig.row("build2")?.is_none());
    Ok(())
}

#[test]
fn a_failed_build2_goes_to_the_cto() -> R {
    let mut rig = build("rework1")?;
    rig.registry
        .node_handler(FlowKind::Build, "build2", Box::new(Fanout("failed")));
    finish(&mut rig, "rework1", &agent_receipt(0, REWORK_SLICED))?;
    assert_cto(&mut rig, "build2", "failed")?;
    assert!(rig.row("audit2")?.is_none());
    assert!(rig.row("test2")?.is_none());
    Ok(())
}

#[test]
fn an_unchanged_build2_goes_to_the_cto() -> R {
    let mut rig = build("rework1")?;
    rig.registry
        .node_handler(FlowKind::Build, "build2", Box::new(Fanout("unchanged")));
    finish(&mut rig, "rework1", &agent_receipt(0, REWORK_SLICED))?;
    assert_cto(&mut rig, "build2", "unchanged")?;
    assert!(rig.row("audit2")?.is_none());
    assert!(rig.row("test2")?.is_none());
    Ok(())
}

#[test]
fn a_verified_join2_opens_the_merge() -> R {
    let mut rig = pass2(CLEAN, &passing())?;
    open(&mut rig, "merge")?;
    assert_eq!(verdict(&rig, "audit2")?.as_deref(), Some("clean_pass"));
    assert_eq!(verdict(&rig, "test2")?.as_deref(), Some("pass"));
    assert!(rig.row("judge2")?.is_none());
    assert_live_quiet(&rig)
}

#[test]
fn a_failing_rung_at_join2_goes_to_the_judge() -> R {
    let mut rig = pass2(CLEAN, &failing())?;
    open(&mut rig, "judge2")?;
    assert!(rig.row("merge")?.is_none());
    assert_live_quiet(&rig)
}

#[test]
fn a_blocking_audit_at_join2_goes_to_the_cto() -> R {
    let mut rig = pass2(BLOCK, &passing())?;
    assert_cto(&mut rig, "audit2", "block_range")?;
    assert!(rig.row("merge")?.is_none());
    Ok(())
}

#[test]
fn a_lost_rung_at_join2_goes_to_the_cto() -> R {
    let mut rig = build("rework1")?;
    to_pass2(&mut rig)?;
    finish(&mut rig, "audit2", &agent_receipt(0, CLEAN))?;
    lose_twice(&mut rig, "test2")?;
    assert_cto(&mut rig, "test2", "failed")?;
    assert!(rig.row("merge")?.is_none());
    Ok(())
}

#[test]
fn an_audit_that_raises_the_rung_sends_join2_to_raise2() -> R {
    let mut rig = pass2(RAISES_MAIN, &passing())?;
    open(&mut rig, "raise2")?;
    let command = rig.open_job("raise2").and_then(|job| job.command);
    assert_eq!(command.as_deref(), Some("cargo test --all"));
    assert!(rig.row("merge")?.is_none());
    assert_live_quiet(&rig)
}

#[test]
fn a_residual_judge2_without_a_raise_goes_to_the_merge() -> R {
    let mut rig = pass2(CLEAN, &failing())?;
    finish(&mut rig, "judge2", &agent_receipt(0, JUDGE_RESIDUAL))?;
    open(&mut rig, "merge")?;
    assert_eq!(verdict(&rig, "judge2")?.as_deref(), Some("residual"));
    assert!(rig.row("raise2")?.is_none());
    assert_live_quiet(&rig)
}

#[test]
fn a_residual_judge2_after_a_raising_audit_goes_to_raise2() -> R {
    let mut rig = pass2(RAISES_MAIN, &failing())?;
    finish(&mut rig, "judge2", &agent_receipt(0, JUDGE_RESIDUAL))?;
    open(&mut rig, "raise2")?;
    assert_eq!(verdict(&rig, "judge2")?.as_deref(), Some("residual"));
    assert!(rig.row("merge")?.is_none());
    assert_live_quiet(&rig)
}

#[test]
fn a_blocking_judge2_goes_to_the_cto() -> R {
    let mut rig = pass2(CLEAN, &failing())?;
    finish(&mut rig, "judge2", &agent_receipt(0, JUDGE_BLOCK))?;
    assert_cto(&mut rig, "judge2", "block")?;
    assert!(rig.row("merge")?.is_none());
    Ok(())
}

#[test]
fn a_failed_judge2_goes_to_the_cto() -> R {
    let mut rig = pass2(CLEAN, &failing())?;
    lose_twice(&mut rig, "judge2")?;
    assert_cto(&mut rig, "judge2", "failed")?;
    assert!(rig.row("merge")?.is_none());
    Ok(())
}

/// A pass 2 whose audit raises the rung, up to the `raise2` job.
fn to_raise2() -> Result<Rig, VivariumError> {
    let mut rig = pass2(RAISES_MAIN, &passing())?;
    open(&mut rig, "raise2")?;
    Ok(rig)
}

#[test]
fn a_passing_raise2_opens_the_merge() -> R {
    let mut rig = to_raise2()?;
    finish(&mut rig, "raise2", &passing())?;
    open(&mut rig, "merge")?;
    assert_eq!(verdict(&rig, "raise2")?.as_deref(), Some("pass"));
    assert_live_quiet(&rig)
}

#[test]
fn a_failing_raise2_goes_to_the_cto() -> R {
    let mut rig = to_raise2()?;
    finish(&mut rig, "raise2", &failing())?;
    assert_cto(&mut rig, "raise2", "fail")?;
    assert!(rig.row("merge")?.is_none());
    Ok(())
}

#[test]
fn a_lost_raise2_goes_to_the_cto() -> R {
    let mut rig = to_raise2()?;
    lose_twice(&mut rig, "raise2")?;
    assert_cto(&mut rig, "raise2", "failed")?;
    assert!(rig.row("merge")?.is_none());
    Ok(())
}

#[test]
fn a_blocked_first_pass_reworks_and_lands_on_a_clean_second_pass() -> R {
    let mut rig = build("plan")?;
    finish_noted(&mut rig, "plan", &agent_receipt(0, PLANNED))?;
    finish_noted(&mut rig, "admit1", &agent_receipt(0, ADMITTED))?;
    finish_noted(&mut rig, "slice", &agent_receipt(0, SLICED))?;
    rig.tick_until("the audit1 and test1 jobs", |rig| {
        rig.open_job("audit1").is_some() && rig.open_job("test1").is_some()
    })?;
    finish(&mut rig, "audit1", &agent_receipt(0, BLOCK))?;
    finish(&mut rig, "test1", &passing())?;
    finish(&mut rig, "rework1", &agent_receipt(0, REWORK_SLICED))?;
    rig.tick_until("the audit2 and test2 jobs", |rig| {
        rig.open_job("audit2").is_some() && rig.open_job("test2").is_some()
    })?;
    finish(&mut rig, "audit2", &agent_receipt(0, CLEAN))?;
    finish(&mut rig, "test2", &passing())?;
    finish(&mut rig, "merge", &merged_main())?;
    rig.tick_until("run done", run_done)?;
    assert_eq!(verdict(&rig, "merge")?.as_deref(), Some("merged"));
    assert_eq!(rig.runs()?.len(), 1);
    assert_eq!(rig.runs()?[0].state, "done", "{}", rig.exceptions());
    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Done);
    assert_eq!(
        rig.port.last_closed_record(&rig.need)?.as_deref(),
        Some("landed")
    );
    assert_eq!(rig.exceptions(), "");
    Ok(())
}
