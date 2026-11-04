//! The rework1 node override: the shipped registry picks it for rework1 only,
//! and its slicer job brief carries the stored findings and failing output.

use std::collections::BTreeMap;

use serde_json::json;
use tempfile::{TempDir, tempdir};

use crate::pump::config::PumpConfig;
use crate::pump::flow::def::{FlowKind, FlowNode, NodeClass};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::{Registry, shipped};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::step_inputs::POOL_RESULT_MARKER;
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::jobs::{JobSpec, JobStore, PoolJob};
use crate::pump::store::step_payload::StepPayloadRow;
use crate::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};
use vivi_error::VivariumError;

type R = Result<(), VivariumError>;

const NEED: &str = "n1";
const BASE: &str = "0123456789abcdef0123456789abcdef01234567";

#[derive(Default)]
struct Jobs {
    jobs: BTreeMap<String, PoolJob>,
}

impl JobStore for Jobs {
    fn jobs(&self) -> &BTreeMap<String, PoolJob> {
        &self.jobs
    }

    fn jobs_mut(&mut self) -> &mut BTreeMap<String, PoolJob> {
        &mut self.jobs
    }
}

/// Answers a marker word, to tell the slice node's handler apart.
struct Marker;

impl StepHandler for Marker {
    fn start(&self, _cx: &StartCx<'_>, _step: &StepRow) -> Result<StartAction, VivariumError> {
        Ok(StartAction::Decide {
            word: "marker".into(),
        })
    }
}

struct Rig {
    _dir: TempDir,
    ctx: Ctx,
    store: PumpStore,
    mail: FakeMail,
    run: RunRow,
}

fn rig() -> Result<Rig, VivariumError> {
    rig_with(PumpConfig::default())
}

fn rig_with(cfg: PumpConfig) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, _host) = test_ctx(dir.path(), cfg);
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    store.upsert_need_branch(&NeedBranchRow {
        need: NEED.into(),
        branch: format!("vivi/need/{NEED}"),
        base: Some(BASE.into()),
        status: "live".into(),
        run: 1,
        target: "default".into(),
        priors: Vec::new(),
    })?;
    Ok(Rig {
        _dir: dir,
        ctx,
        store,
        mail: FakeMail::new("mind"),
        run: RunRow {
            need: NEED.into(),
            run: 1,
            flow: "build".into(),
            flow_version: 2,
            kind: "build".into(),
            entry: "plan".into(),
            state: "live".into(),
        },
    })
}

fn step(node: &str, task: &str, verdict: Option<&str>) -> StepRow {
    StepRow {
        run: 1,
        node: node.into(),
        task: task.into(),
        state: StepState::Done,
        verdict: verdict.map(str::to_string),
    }
}

fn payload(class: &str, node: &str, task: &str, body: serde_json::Value) -> StepPayloadRow {
    StepPayloadRow {
        need: NEED.into(),
        run: 1,
        node: node.into(),
        class: class.into(),
        task: task.into(),
        payload: body,
    }
}

impl Rig {
    /// A run with an admitted plan, one merged slice, an audit with a finding
    /// and a gap, a judge reason and a failing need rung.
    fn seed(&self) -> R {
        let rows = [
            ("plan", "planner", "t-plan", Some("planned")),
            ("audit1", "auditor", "t-audit", Some("block")),
            ("judge1", "judge", "t-judge", Some("block")),
            ("test1", "rung-need", "t-rung", Some("fail")),
        ];
        for (node, _, task, verdict) in rows {
            self.store.insert_step(NEED, &step(node, task, verdict))?;
        }
        self.store
            .insert_step(NEED, &step("slice:s1", "t-s1", Some("merged")))?;
        self.store
            .insert_step(NEED, &step("slice:s2", "t-s2", Some("failed")))?;
        let bodies = [
            payload("planner", "plan", "t-plan", json!({ "body": "THE PLAN" })),
            payload(
                "auditor",
                "audit1",
                "t-audit",
                json!({ "front": {
                    "answer": "block_range",
                    "finding": [{
                        "id": "F-77",
                        "content": "the cache is never cleared",
                        "paths": ["src/cache.rs"],
                        "recheck_trigger": "cache edits",
                    }],
                    "requirement": [
                        { "id": "R1", "text": "clears the cache", "status": "MISSING", "evidence": [] },
                        { "id": "R2", "text": "builds", "status": "SATISFIED", "evidence": [] },
                    ],
                } }),
            ),
            payload(
                "judge",
                "judge1",
                "t-judge",
                json!({ "front": { "answer": "block", "reason": "JUDGE SAYS BLOCK" } }),
            ),
            payload(
                "rung-need",
                "test1",
                "t-rung",
                json!({ "exit": 101, "tail": "boom", "failing": ["emit_harness::emit::a"] }),
            ),
        ];
        for body in &bodies {
            self.store.record_step_payload(body)?;
        }
        Ok(())
    }
}

#[test]
fn the_shipped_registry_picks_the_override_for_rework1_and_not_for_slice() -> R {
    let rig = rig()?;
    rig.seed()?;
    let mut registry = shipped::<Jobs, FakeMail>(&rig.ctx);
    registry.node_handler(FlowKind::Build, "slice", Box::new(Marker));
    let def = load(FlowKind::Build)?;
    let cx = StartCx {
        ctx: &rig.ctx,
        store: &rig.store,
        mail: &rig.mail,
        run: &rig.run,
        def: &def,
    };
    let node = |id: &str| FlowNode {
        id: id.into(),
        class: NodeClass::Slicer,
        label: String::new(),
    };

    let slice = registry
        .handler_for(FlowKind::Build, &node("slice"))
        .ok_or_else(|| VivariumError::Other("no handler for slice".into()))?;
    assert_eq!(
        slice.start(&cx, &step("slice", "t-x", None))?,
        StartAction::Decide {
            word: "marker".into()
        }
    );

    let rework = registry
        .handler_for(FlowKind::Build, &node("rework1"))
        .ok_or_else(|| VivariumError::Other("no handler for rework1".into()))?;
    let StartAction::Job(job) = rework.start(&cx, &step("rework1", "t-rw", None))? else {
        return Err(VivariumError::Other("rework1 did not start a job".into()));
    };
    assert_eq!(job.role, "slicer");
    assert!(job.read_only);
    assert_eq!(job.from_ref.as_deref(), Some(rig.ctx.cfg.main.as_str()));
    assert_eq!(
        job.extra_targets,
        crate::pump::flow::step_inputs::targets(&rig.ctx)
    );
    assert_eq!(job.kind, "flow-rework");
    assert_eq!(job.task.as_deref(), Some("t-rw"));
    Ok(())
}

#[test]
fn the_rework_brief_carries_the_stored_finding_and_failing_test() -> R {
    let rig = rig()?;
    rig.seed()?;
    let mut registry: Registry<Jobs, FakeMail> = Registry::new();
    super::register(&mut registry);
    let def = load(FlowKind::Build)?;
    let cx = StartCx {
        ctx: &rig.ctx,
        store: &rig.store,
        mail: &rig.mail,
        run: &rig.run,
        def: &def,
    };
    let node = FlowNode {
        id: "rework1".into(),
        class: NodeClass::Slicer,
        label: String::new(),
    };
    let handler = registry
        .handler_for(FlowKind::Build, &node)
        .ok_or_else(|| VivariumError::Other("rework1 not registered".into()))?;
    let StartAction::Job(job) = handler.start(&cx, &step("rework1", "t-rw", None))? else {
        return Err(VivariumError::Other("rework1 did not start a job".into()));
    };
    let body = &job.body;
    assert!(body.starts_with("mode: rework\n"), "{body}");
    assert!(body.contains("need: n1"), "{body}");
    assert!(body.contains("vivi/need/n1"), "{body}");
    assert!(body.contains("THE PLAN"), "{body}");
    assert!(body.contains("slice:s1 (task t-s1)"), "{body}");
    let merged = body
        .split("## Slices already merged on the need branch")
        .nth(1)
        .and_then(|rest| rest.split("## Slices that did not land").next())
        .unwrap_or_default();
    assert!(
        !merged.contains("t-s2"),
        "failed slice listed as merged: {body}"
    );
    assert!(
        body.contains("(task t-s2, failed)"),
        "failed slice not named: {body}"
    );
    assert!(body.contains("F-77"), "{body}");
    assert!(body.contains("src/cache.rs"), "{body}");
    assert!(body.contains("requirement R1 [MISSING]"), "{body}");
    assert!(!body.contains("R2"), "satisfied requirement listed: {body}");
    assert!(body.contains("JUDGE SAYS BLOCK"), "{body}");
    assert!(body.contains("emit_harness::emit::a"), "{body}");
    assert!(body.contains("exit: 101"), "{body}");
    Ok(())
}

#[test]
fn a_rework_restart_run_reads_the_run_that_ended_at_the_cto() -> R {
    let mut rig = rig()?;
    rig.seed()?;
    rig.store
        .insert_step(NEED, &step("cto", "t-cto", Some("rework")))?;
    rig.store.record_step_payload(&payload(
        "cto",
        "cto",
        "t-cto",
        json!({ "front": {
            "answer": "rework",
            "reason": "R-WHY",
            "new_fact": "X-FACT",
        } }),
    ))?;
    rig.run.run = 2;
    rig.run.entry = "rework1".into();
    let body = rework_brief(&rig)?;
    assert!(body.contains("context: run 1"), "{body}");
    assert!(body.contains("F-77"), "{body}");
    assert!(body.contains("emit_harness::emit::a"), "{body}");
    assert!(body.contains("slice:s1 (task t-s1)"), "{body}");
    assert!(body.contains("## CTO ruling"), "{body}");
    assert!(body.contains("reason: R-WHY"), "{body}");
    assert!(body.contains("new_fact: X-FACT"), "{body}");

    Ok(())
}

#[test]
fn a_rework_after_a_failed_build_lists_the_slices_the_kept_branch_holds() -> R {
    let mut rig = rig()?;
    // Run 1 built no audit: one slice merged, one ran out of attempts, and
    // the CTO answered rework, which keeps the need branch.
    rig.store.insert_run(&rig.run)?;
    rig.store
        .insert_step(NEED, &step("slice:s1", "t-s1", Some("merged")))?;
    rig.store
        .insert_step(NEED, &step("slice:s2", "t-s2", Some("failed")))?;
    rig.store
        .insert_step(NEED, &step("cto", "t-cto", Some("rework")))?;
    rig.run.run = 2;
    rig.run.entry = "rework1".into();
    rig.store.insert_run(&rig.run)?;
    let body = rework_brief(&rig)?;
    assert!(body.contains("slice:s1 (task t-s1)"), "{body}");
    assert!(!body.contains("slice:s2 (task t-s2)"), "{body}");
    assert!(body.contains("### slice:s2 (task t-s2, failed)"), "{body}");
    Ok(())
}

/// The rework of the 2026-11-04 local pump run (need cdf403aa r2): a README
/// slice ran out of attempts because the command it documents had not landed,
/// no audit ran, and head-cto answered rework. The brief must name the slice
/// that did not land with its hand's last message, and the CTO ruling.
#[test]
fn a_rework_after_an_exhausted_slice_names_it_with_its_hand_message_and_the_ruling() -> R {
    let mut rig = rig()?;
    rig.store.insert_run(&rig.run)?;
    rig.store
        .insert_step(NEED, &step("slice:s1", "t-s1", Some("merged")))?;
    let readme = rig
        .mail
        .send_step("pump", "pump", "slice:s3", "body", &[])?;
    rig.mail.reply(
        &readme,
        &format!(
            "{POOL_RESULT_MARKER}j-s3: finished\npool p  slot 1  model m\n\n--- receipt\n\
             The stats command does not exist on this checkout; once it lands the README edits can be made."
        ),
    )?;
    rig.store
        .insert_step(NEED, &step("slice:s3", &readme, Some("superseded")))?;
    rig.store
        .insert_step(NEED, &step("cto", "t-cto", Some("rework")))?;
    rig.store.record_step_payload(&payload(
        "cto",
        "cto",
        "t-cto",
        json!({ "front": { "answer": "rework", "reason": "README-AFTER-STATS" } }),
    ))?;
    rig.run.run = 2;
    rig.run.entry = "rework1".into();
    rig.store.insert_run(&rig.run)?;
    let body = rework_brief(&rig)?;
    assert!(body.contains("## Slices that did not land"), "{body}");
    assert!(
        body.contains(&format!("### slice:s3 (task {readme}, superseded)")),
        "{body}"
    );
    assert!(body.contains("does not exist on this checkout"), "{body}");
    assert!(body.contains("## CTO ruling"), "{body}");
    assert!(body.contains("reason: README-AFTER-STATS"), "{body}");
    Ok(())
}

#[test]
fn an_unlock_restart_names_the_missing_cto_ruling() -> R {
    let mut rig = rig()?;
    rig.seed()?;
    rig.run.run = 2;
    rig.run.entry = "rework1".into();
    let body = rework_brief(&rig)?;
    assert!(body.contains("context: run 1"), "{body}");
    assert!(
        body.contains("(no CTO ruling; an operator unlock"),
        "{body}"
    );
    Ok(())
}

/// The rework1 job the shipped registry starts, for budget assertions.
fn shipped_rework_job(rig: &Rig) -> Result<Box<JobSpec>, VivariumError> {
    let registry = shipped::<Jobs, FakeMail>(&rig.ctx);
    let def = load(FlowKind::Build)?;
    let cx = StartCx {
        ctx: &rig.ctx,
        store: &rig.store,
        mail: &rig.mail,
        run: &rig.run,
        def: &def,
    };
    let node = FlowNode {
        id: "rework1".into(),
        class: NodeClass::Slicer,
        label: String::new(),
    };
    let handler = registry
        .handler_for(FlowKind::Build, &node)
        .ok_or_else(|| VivariumError::Other("no handler for rework1".into()))?;
    match handler.start(&cx, &step("rework1", "t-rw", None))? {
        StartAction::Job(job) => Ok(job),
        other => Err(VivariumError::Other(format!(
            "rework1 did not start a job: {other:?}"
        ))),
    }
}

#[test]
fn rework1_takes_the_configured_slicer_budget_despite_the_flow_rework_label() -> R {
    let mut cfg = PumpConfig::default();
    cfg.dispatch.budget = 222;
    cfg.dispatch.step_budgets.slicer = Some(300);
    // Another thinking kind configured: it must not reach the rework node.
    cfg.dispatch.step_budgets.judge = Some(88);
    let rig = rig_with(cfg)?;
    rig.seed()?;

    let job = shipped_rework_job(&rig)?;

    // The durable kind label stays `flow-rework`; the budget is the slicer's
    // own override, not the shared fallback and not another kind's.
    assert_eq!(job.kind, "flow-rework");
    assert_eq!(job.budget, 300);
    Ok(())
}

#[test]
fn rework1_without_a_slicer_override_takes_the_changed_dispatch_fallback() -> R {
    let mut cfg = PumpConfig::default();
    cfg.dispatch.budget = 222;
    // A thinking override for an unrelated kind must not reach the rework
    // node either.
    cfg.dispatch.step_budgets.planner = Some(90);
    let rig = rig_with(cfg)?;
    rig.seed()?;

    let job = shipped_rework_job(&rig)?;

    assert_eq!(job.budget, 222);
    assert_eq!(job.budget, rig.ctx.cfg.dispatch.budget);
    Ok(())
}

fn rework_brief(rig: &Rig) -> Result<String, VivariumError> {
    let mut registry: Registry<Jobs, FakeMail> = Registry::new();
    super::register(&mut registry);
    let def = load(FlowKind::Build)?;
    let cx = StartCx {
        ctx: &rig.ctx,
        store: &rig.store,
        mail: &rig.mail,
        run: &rig.run,
        def: &def,
    };
    let node = FlowNode {
        id: "rework1".into(),
        class: NodeClass::Slicer,
        label: String::new(),
    };
    let handler = registry
        .handler_for(FlowKind::Build, &node)
        .ok_or_else(|| VivariumError::Other("rework1 not registered".into()))?;
    let StartAction::Job(job) = handler.start(&cx, &step("rework1", "t-rw", None))? else {
        return Err(VivariumError::Other("rework1 did not start a job".into()));
    };
    Ok(job.body.clone())
}

/// The lab stress run (need 3e5bac51): slice n4u4 conflicted and was
/// superseded in run 1, landed in the run 2 rework, and the need went red
/// again. The run 3 rework brief listed n4u4 as merged and as not landed, the
/// slicer cut it again, its hand found nothing to change, and the CTO locked
/// the need. A slice a later run landed is merged only.
#[test]
fn a_slice_a_later_run_landed_is_not_listed_as_not_landed() -> R {
    let mut rig = rig()?;
    rig.store.insert_run(&rig.run)?;
    rig.store
        .insert_step(NEED, &step("slice:s4", "t-s4-r1", Some("superseded")))?;
    rig.run.run = 2;
    rig.run.entry = "rework1".into();
    rig.store.insert_run(&rig.run)?;
    rig.store.insert_step(
        NEED,
        &StepRow {
            run: 2,
            ..step("slice:s4", "t-s4-r2", Some("merged"))
        },
    )?;
    rig.run.run = 3;
    rig.store.insert_run(&rig.run)?;
    let body = rework_brief(&rig)?;
    assert!(body.contains("- slice:s4 (task t-s4-r2)"), "{body}");
    let unlanded = body
        .split("## Slices that did not land")
        .nth(1)
        .and_then(|rest| rest.split("\n## ").next())
        .unwrap_or_default();
    assert!(
        !unlanded.contains("slice:s4"),
        "landed slice listed as not landed: {body}"
    );
    Ok(())
}
