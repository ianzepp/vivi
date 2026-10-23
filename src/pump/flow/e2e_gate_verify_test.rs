//! End to end gate: the fanout and verify pass 1 edges.
//!
//! Region: `build1`, `audit1`, `test1`, `join1`, `judge1`, the raise check and
//! `raise1`. Edges covered: build1 `failed` and `unchanged`; join1 `judge`,
//! `raised`, `block`, `failed`; judge1 `residual`, `block`, `failed`; the raise
//! check `none` and `raised`; raise1 `pass`, `fail`, `failed`.
//!
//! Every test runs a build need on a real mailspace through `tick_with` with
//! the shipped registry, started at `slice`. The slicer step and the fanout
//! handler are replaced by stubs that answer a fixed word at once (the front
//! and the fanout have their own tests; `rework1` is the real slicer step);
//! the test finishes the pool jobs the pump opened, as the pool would, and
//! ticks. `built` (to `audit1` and `test1`) and `verified` (to `merge`) are
//! covered by `e2e_gate_full_test.rs`.

use super::e2e_rig::{Rig, agent_receipt, rig};
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

const REWORK_SLICED: &str = "+++\nanswer = \"sliced\"\nmode = \"rework\"\n\n[[slice]]\n\
                             id = \"r\"\ntitle = 'Fix'\nbrief = 'fix it'\ntarget = \"default\"\n\
                             write_scope = [\"src/a.rs\"]\n+++\ncut one rework slice";
const CLEAN: &str = "+++\nanswer = \"clean_pass\"\n+++\nclean";
const RAISES_MAIN: &str = "+++\nanswer = \"residual\"\nraise_rung = \"main\"\n+++\nrun main";
const BLOCK_RANGE: &str = "+++\nanswer = \"block_range\"\n+++\nbroken range";
const BLOCK_SHIP: &str = "+++\nanswer = \"block_ship\"\n+++\nnot shippable";
const JUDGE_RESIDUAL: &str =
    "+++\nanswer = \"residual\"\nreason = 'minor leftovers'\n+++\nresidual";
const JUDGE_BLOCK: &str = "+++\nanswer = \"block\"\nreason = 'the failure is real'\n+++\nblock";

/// Stands in for a handler: its step answers `word` at once.
struct Answers(&'static str);

impl StepHandler for Answers {
    fn start(&self, _cx: &StartCx<'_>, _step: &StepRow) -> Result<StartAction, VivariumError> {
        Ok(StartAction::Decide {
            word: self.0.into(),
        })
    }
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

/// A build run started at `slice`, over a live need branch at [`TIP`] cut from
/// [`BASE`], with main at [`MAIN`], a merge role routed and the fanout stubbed
/// to answer `fanout`.
fn build(fanout: &'static str) -> Result<Rig, VivariumError> {
    let mut rig = rig(config(), "kind: build", "slice")?;
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
        .node_handler(FlowKind::Build, "slice", Box::new(Answers("sliced")));
    rig.registry.handler(
        FlowKind::Build,
        NodeClass::Fanout,
        Box::new(Answers(fanout)),
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

/// Wait until the stubs answered `sliced` and `built` and the audit and the
/// need rung are both open.
fn to_verify(rig: &mut Rig) -> R {
    rig.tick_until("the audit1 and test1 jobs", |rig| {
        rig.open_job("audit1").is_some() && rig.open_job("test1").is_some()
    })
}

/// Pass 1 with `audit` as the audit envelope and the need rung at exit
/// `rung_exit`, up to the join's answer.
fn verify(rig: &mut Rig, audit: &str, rung_exit: i32) -> R {
    to_verify(rig)?;
    finish(rig, "audit1", &agent_receipt(0, audit))?;
    let note = if rung_exit == 0 {
        "all tests passed"
    } else {
        "test x ... FAILED"
    };
    finish(rig, "test1", &agent_receipt(rung_exit, note))
}

/// Pass 1 with the given audit and a failing need rung, up to the `judge1` job.
fn to_judge(rig: &mut Rig, audit: &str) -> R {
    verify(rig, audit, 1)?;
    open(rig, "judge1").map(drop)
}

/// The CTO job opened after `node` answered `word`; the run is still live and
/// no alarm was raised.
fn assert_cto(rig: &mut Rig, node: &str, word: &str) -> R {
    open(rig, "cto")?;
    assert_eq!(verdict(rig, node)?.as_deref(), Some(word));
    assert_eq!(rig.runs()?.len(), 1);
    assert_eq!(rig.runs()?[0].state, "live");
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

/// The `rework1` slicer job opened in rework mode after `node` answered
/// `word`; no alarm was raised.
fn assert_rework(rig: &mut Rig, node: &str, word: &str) -> R {
    let id = open(rig, "rework1")?;
    assert_eq!(verdict(rig, node)?.as_deref(), Some(word));
    let body = rig.jobs.jobs[&id].body.clone();
    assert!(body.starts_with("mode: rework"), "{body}");
    assert!(rig.row("cto")?.is_none());
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

/// The `merge` job opened after `node` answered `word`; no alarm was raised.
fn assert_merge(rig: &mut Rig, node: &str, word: &str) -> R {
    open(rig, "merge")?;
    assert_eq!(verdict(rig, node)?.as_deref(), Some(word));
    assert!(rig.row("cto")?.is_none());
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn a_failed_build1_goes_to_the_cto() -> R {
    let mut rig = build("failed")?;
    assert_cto(&mut rig, "build1", "failed")?;
    assert!(rig.row("audit1")?.is_none());
    assert!(rig.row("test1")?.is_none());
    Ok(())
}

#[test]
fn an_unchanged_build1_goes_to_the_cto() -> R {
    let mut rig = build("unchanged")?;
    assert_cto(&mut rig, "build1", "unchanged")?;
    assert!(rig.row("audit1")?.is_none());
    assert!(rig.row("test1")?.is_none());
    Ok(())
}

#[test]
fn a_clean_audit_with_a_failing_rung_joins_to_judge_and_opens_judge1() -> R {
    let mut rig = build("built")?;
    verify(&mut rig, CLEAN, 1)?;
    open(&mut rig, "judge1")?;
    assert_eq!(verdict(&rig, "audit1")?.as_deref(), Some("clean_pass"));
    assert_eq!(verdict(&rig, "test1")?.as_deref(), Some("fail"));
    assert!(rig.row("merge")?.is_none());
    assert!(rig.row("rework1")?.is_none());
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn a_residual_judge_with_no_raise_goes_through_the_raise_check_to_merge() -> R {
    let mut rig = build("built")?;
    to_judge(&mut rig, CLEAN)?;
    finish(&mut rig, "judge1", &agent_receipt(0, JUDGE_RESIDUAL))?;
    assert_merge(&mut rig, "judge1", "residual")?;
    assert!(rig.row("raise1")?.is_none());
    Ok(())
}

#[test]
fn a_residual_judge_after_a_raising_audit_goes_through_the_raise_check_to_raise1() -> R {
    let mut rig = build("built")?;
    to_judge(&mut rig, RAISES_MAIN)?;
    finish(&mut rig, "judge1", &agent_receipt(0, JUDGE_RESIDUAL))?;
    let id = open(&mut rig, "raise1")?;
    assert_eq!(verdict(&rig, "judge1")?.as_deref(), Some("residual"));
    assert_eq!(
        rig.jobs.jobs[&id].command.as_deref(),
        Some("cargo test --all")
    );
    assert!(rig.row("merge")?.is_none());
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn a_block_judge_goes_to_rework1() -> R {
    let mut rig = build("built")?;
    to_judge(&mut rig, CLEAN)?;
    finish(&mut rig, "judge1", &agent_receipt(0, JUDGE_BLOCK))?;
    assert_rework(&mut rig, "judge1", "block")?;
    assert!(rig.row("raise1")?.is_none());
    Ok(())
}

#[test]
fn a_failed_judge1_goes_to_the_cto() -> R {
    let mut rig = build("built")?;
    to_judge(&mut rig, CLEAN)?;
    lose_twice(&mut rig, "judge1")?;
    assert_cto(&mut rig, "judge1", "failed")?;
    assert!(rig.row("rework1")?.is_none());
    Ok(())
}

#[test]
fn a_raising_audit_with_a_passing_rung_joins_to_raised_and_opens_raise1() -> R {
    let mut rig = build("built")?;
    verify(&mut rig, RAISES_MAIN, 0)?;
    let id = open(&mut rig, "raise1")?;
    assert_eq!(verdict(&rig, "audit1")?.as_deref(), Some("residual"));
    assert_eq!(verdict(&rig, "test1")?.as_deref(), Some("pass"));
    assert_eq!(
        rig.jobs.jobs[&id].command.as_deref(),
        Some("cargo test --all")
    );
    assert!(rig.row("judge1")?.is_none());
    assert!(rig.row("merge")?.is_none());
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn a_passing_raise1_goes_to_merge() -> R {
    let mut rig = build("built")?;
    verify(&mut rig, RAISES_MAIN, 0)?;
    finish(&mut rig, "raise1", &agent_receipt(0, "all tests passed"))?;
    assert_merge(&mut rig, "raise1", "pass")
}

#[test]
fn a_failing_raise1_goes_to_rework1() -> R {
    let mut rig = build("built")?;
    verify(&mut rig, RAISES_MAIN, 0)?;
    finish(&mut rig, "raise1", &agent_receipt(1, "test x ... FAILED"))?;
    assert_rework(&mut rig, "raise1", "fail")
}

#[test]
fn a_failed_raise1_goes_to_the_cto() -> R {
    let mut rig = build("built")?;
    verify(&mut rig, RAISES_MAIN, 0)?;
    lose_twice(&mut rig, "raise1")?;
    assert_cto(&mut rig, "raise1", "failed")?;
    assert!(rig.row("merge")?.is_none());
    Ok(())
}

#[test]
fn a_block_range_audit_joins_to_block_and_goes_to_rework1() -> R {
    let mut rig = build("built")?;
    verify(&mut rig, BLOCK_RANGE, 0)?;
    assert_rework(&mut rig, "audit1", "block_range")?;
    assert!(rig.row("judge1")?.is_none());
    Ok(())
}

#[test]
fn a_block_ship_audit_joins_to_block_and_goes_to_rework1() -> R {
    let mut rig = build("built")?;
    verify(&mut rig, BLOCK_SHIP, 0)?;
    assert_rework(&mut rig, "audit1", "block_ship")?;
    assert!(rig.row("judge1")?.is_none());
    Ok(())
}

#[test]
fn a_failed_audit_joins_to_failed_and_goes_to_the_cto() -> R {
    let mut rig = build("built")?;
    to_verify(&mut rig)?;
    lose_twice(&mut rig, "audit1")?;
    finish(&mut rig, "test1", &agent_receipt(0, "all tests passed"))?;
    assert_cto(&mut rig, "audit1", "failed")?;
    assert!(rig.row("merge")?.is_none());
    Ok(())
}

#[test]
fn a_rework_slice_reply_after_pass_1_opens_build2() -> R {
    let mut rig = build("built")?;
    verify(&mut rig, BLOCK_RANGE, 0)?;
    finish(&mut rig, "rework1", &agent_receipt(0, REWORK_SLICED))?;
    rig.tick_until("build2", |rig| {
        rig.row("build2").is_ok_and(|row| row.is_some())
    })?;
    assert_eq!(rig.exceptions(), "");
    Ok(())
}
