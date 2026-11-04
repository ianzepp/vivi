//! End to end gate: build-flow stages switched off in `[pump.flow.stages]`.
//!
//! A switched-off stage starts no job: its step answers at once by rule and
//! records the skip in its payload. Covered: `admit` off (admit1 answers
//! `admitted`), `audit` off (audit1 answers `skipped`, the join reads it as no
//! findings), `need_rung` off (test1 answers `skipped`), both off (the build
//! goes straight to the merge: the merge job opening is the join's `verified`), and `judge` off (a red need rung goes to
//! rework with no judge job). An auditor that answers `skipped` itself is
//! refused: the word is not an auditor answer, so only a switch gives it.
//!
//! Every test runs a build need on a real mailspace through the real tick, as
//! the other e2e gate tests do; the pass-1 tests start at `slice` with the
//! slicer and the fanout stubbed.

use super::e2e_rig::{Rig, agent_receipt, rig};
use crate::pump::config::{PumpConfig, RungsConfig, StagesConfig};
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::handlers::shipped;
use crate::pump::flow::index::StepRow;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::jobs::JobStatus;
use crate::pump::store::NeedBranchRow;
use crate::routing::RoutedRole;
use vivi_error::VivariumError;

type R = Result<(), VivariumError>;

const BASE: &str = "1111111111111111111111111111111111111111";
const TIP: &str = "2222222222222222222222222222222222222222";
const MAIN: &str = "3333333333333333333333333333333333333333";
const CLEAN: &str = "+++\nanswer = \"clean_pass\"\n+++\nclean";

/// Stands in for a handler: its step answers `word` at once.
struct Answers(&'static str);

impl StepHandler for Answers {
    fn start(&self, _cx: &StartCx<'_>, _step: &StepRow) -> Result<StartAction, VivariumError> {
        Ok(StartAction::Decide {
            word: self.0.into(),
        })
    }
}

fn config(stages: StagesConfig) -> PumpConfig {
    let mut cfg = PumpConfig::default();
    cfg.rungs = RungsConfig {
        need: Some(String::from("cargo test")),
        main: Some(String::from("cargo test --all")),
        timeout: Some(900),
        pools: vec![String::from("test-planner-pool")],
        ..RungsConfig::default()
    };
    cfg.flow.stages = stages;
    cfg
}

fn route(mut rig: Rig, roles: &[(&str, Option<bool>)]) -> Rig {
    let mut routing = (*rig.ctx.routing).clone();
    for (name, read_only) in roles {
        routing.roles.push(RoutedRole {
            name: (*name).into(),
            kind: Some((*name).into()),
            status: String::from("active"),
            pool: Some(String::from("test-planner-pool")),
            read_only: *read_only,
            model: None,
            models: Vec::new(),
        });
    }
    rig.ctx = rig.ctx.with_routing(routing);
    rig.registry = shipped(&rig.ctx);
    rig
}

fn need_thread(rig: &Rig) {
    let shown = serde_json::json!([
        {"handle": rig.need, "message_id": "m1", "from": "operator@local",
         "subject": "Need", "body": vivi_mail::letter::fixture::need_body(&["it works"], "Build the thing")}
    ]);
    rig.project.host.reply("need show", 0, &shown.to_string());
}

/// A build run started at `slice` with the slicer and the fanout stubbed to
/// answer `sliced` and `built`, over a live need branch.
fn pass1(stages: StagesConfig) -> Result<Rig, VivariumError> {
    let rig = rig(config(stages), "+++\nkind = \"build\"\n+++\n", "slice")?;
    let mut rig = route(rig, &[("merge", Some(false))]);
    rig.registry
        .node_handler(FlowKind::Build, "slice", Box::new(Answers("sliced")));
    rig.registry.handler(
        FlowKind::Build,
        NodeClass::Fanout,
        Box::new(Answers("built")),
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
    need_thread(&rig);
    Ok(rig)
}

fn open(rig: &mut Rig, node: &str) -> Result<String, VivariumError> {
    rig.tick_until(&format!("a {node} job"), |rig| rig.open_job(node).is_some())?;
    rig.open_job(node)
        .map(|job| job.id)
        .ok_or_else(|| VivariumError::Message(format!("no open {node} job")))
}

fn finish(rig: &mut Rig, node: &str, receipt: &str) -> R {
    let id = open(rig, node)?;
    rig.finish(&id, JobStatus::Finished, receipt)
}

fn verdict(rig: &Rig, node: &str) -> Result<Option<String>, VivariumError> {
    Ok(rig.row(node)?.and_then(|row| row.verdict))
}

/// The stored payload of `node` records a config skip of `stage`.
fn assert_skipped(rig: &Rig, node: &str, stage: &str) -> R {
    let task = rig
        .row(node)?
        .map(|row| row.task)
        .ok_or_else(|| VivariumError::Message(format!("no {node} row")))?;
    let rows = rig.store.step_payloads_of_run(&rig.need, 1)?;
    let payload = &rows
        .iter()
        .find(|row| row.task == task)
        .ok_or_else(|| VivariumError::Message(format!("no {node} payload")))?
        .payload;
    assert_eq!(payload["skipped"], "config", "{payload}");
    assert_eq!(payload["stage"], stage, "{payload}");
    Ok(())
}

#[test]
fn audit_off_skips_the_audit_and_the_rung_alone_decides() -> R {
    let mut rig = pass1(StagesConfig {
        audit: Some(false),
        ..StagesConfig::default()
    })?;
    finish(&mut rig, "test1", &agent_receipt(0, "all tests passed"))?;
    open(&mut rig, "merge")?;
    assert_eq!(verdict(&rig, "audit1")?.as_deref(), Some("skipped"));
    assert!(rig.open_job("audit1").is_none());
    assert_skipped(&rig, "audit1", "audit")?;
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn need_rung_off_skips_the_rung_and_the_audit_alone_decides() -> R {
    let mut rig = pass1(StagesConfig {
        need_rung: Some(false),
        ..StagesConfig::default()
    })?;
    finish(&mut rig, "audit1", &agent_receipt(0, CLEAN))?;
    open(&mut rig, "merge")?;
    assert_eq!(verdict(&rig, "test1")?.as_deref(), Some("skipped"));
    assert_skipped(&rig, "test1", "need_rung")?;
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn audit_and_need_rung_off_go_straight_to_the_merge() -> R {
    let mut rig = pass1(StagesConfig {
        audit: Some(false),
        need_rung: Some(false),
        ..StagesConfig::default()
    })?;
    open(&mut rig, "merge")?;
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn judge_off_sends_a_red_need_rung_to_rework() -> R {
    let rig = pass1(StagesConfig {
        judge: Some(false),
        ..StagesConfig::default()
    })?;
    let mut rig = route(rig, &[("slicer", Some(true))]);
    rig.registry
        .node_handler(FlowKind::Build, "slice", Box::new(Answers("sliced")));
    rig.registry.handler(
        FlowKind::Build,
        NodeClass::Fanout,
        Box::new(Answers("built")),
    );
    finish(&mut rig, "audit1", &agent_receipt(0, CLEAN))?;
    finish(&mut rig, "test1", &agent_receipt(1, "test x ... FAILED"))?;
    open(&mut rig, "rework1")?;
    assert_eq!(verdict(&rig, "judge1")?.as_deref(), Some("block"));
    assert!(rig.open_job("judge1").is_none());
    assert_skipped(&rig, "judge1", "judge")?;
    Ok(())
}

#[test]
fn a_model_answering_skipped_is_refused() -> R {
    let mut rig = pass1(StagesConfig::default())?;
    finish(&mut rig, "test1", &agent_receipt(0, "all tests passed"))?;
    let envelope = "+++\nanswer = \"skipped\"\n+++\nskipped";
    finish(&mut rig, "audit1", &agent_receipt(0, envelope))?;
    for _ in 0..3 {
        rig.tick()?;
    }
    assert_ne!(verdict(&rig, "audit1")?.as_deref(), Some("skipped"));
    assert!(rig.row("merge")?.is_none());
    Ok(())
}

#[test]
fn admit_off_admits_the_plan_and_opens_the_slicer() -> R {
    let stages = StagesConfig {
        admit: Some(false),
        ..StagesConfig::default()
    };
    let rig = rig(config(stages), "+++\nkind = \"build\"\n+++\n", "plan")?;
    let mut rig = route(rig, &[("admitter", Some(true)), ("slicer", Some(true))]);
    need_thread(&rig);
    let id = open(&mut rig, "plan")?;
    let receipt = agent_receipt(0, "+++\nanswer = \"planned\"\n+++\nplanned");
    rig.finish(&id, JobStatus::Finished, &receipt)?;
    // The pool records the settled job on its step task; the slicer reads the
    // plan from there.
    let task = rig
        .store
        .step_job(&id)?
        .map(|row| row.task)
        .ok_or_else(|| VivariumError::Message(format!("job {id} has no step row")))?;
    rig.port.mailspace.lock().record_pool_note(
        &task,
        "pool_collected",
        &format!("{{\"job\":\"{id}\"}}"),
        &receipt,
    )?;
    open(&mut rig, "slice")?;
    assert_eq!(verdict(&rig, "admit1")?.as_deref(), Some("admitted"));
    assert!(rig.open_job("admit1").is_none());
    assert_skipped(&rig, "admit1", "admit")
}
