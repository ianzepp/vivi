//! The rework1 node override: the shipped registry picks it for rework1 only,
//! and its slicer job brief carries the stored findings and failing output.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::json;
use tempfile::{TempDir, tempdir};

use crate::pump::config::{PumpConfig, PumpTarget};
use crate::pump::flow::def::{FlowKind, FlowNode, NodeClass};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::{Registry, shipped};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::step_inputs::POOL_RESULT_MARKER;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobSpec, JobStore, PoolJob};
use crate::pump::store::step_payload::StepPayloadRow;
use crate::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};
use vivi_error::VivariumError;
use vivi_mail::letter::roles::{Slicer, SlicerAnswer, SlicerMode, parse_role_reply};

type R = Result<(), VivariumError>;

const NEED: &str = "n1";
const BASE: &str = "0123456789abcdef0123456789abcdef01234567";
const TIP: &str = "89abcdef0123456789abcdef0123456789abcdef";
/// The target's current main tip.
const MAIN: &str = "c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00";

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
    host: Arc<FakeHost>,
    store: PumpStore,
    mail: FakeMail,
    run: RunRow,
}

fn rig() -> Result<Rig, VivariumError> {
    rig_with(PumpConfig::default())
}

fn rig_with(cfg: PumpConfig) -> Result<Rig, VivariumError> {
    rig_branch(cfg, Some(live_row()))
}

fn live_row() -> NeedBranchRow {
    NeedBranchRow {
        need: NEED.into(),
        branch: format!("vivi/need/{NEED}"),
        base: Some(BASE.into()),
        status: "live".into(),
        run: 1,
        target: "default".into(),
        priors: Vec::new(),
    }
}

/// Put `tip` on the need branch's remote ref, as a published branch has it.
fn publish_tip(rig: &Rig, tip: &str) {
    rig.host.reply(
        "ls-remote origin refs/heads/vivi/need/",
        0,
        &format!("{tip}\trefs/heads/vivi/need/{NEED}\n"),
    );
}

/// The published need branch holds a commit main does not: main is not an
/// ancestor of the tip, so the tip is the immutable base.
fn tip_outside_main(rig: &Rig) {
    rig.host
        .reply(&format!("merge-base --is-ancestor {TIP} {MAIN}"), 1, "");
}

/// `vivi need show` answered with a typed root need declaring `target`.
fn root_need(rig: &Rig, target: Option<&str>) {
    let front = vivi_mail::letter::work::NeedFront {
        target: target.map(str::to_owned),
        ..vivi_mail::letter::work::NeedFront::new(vec![String::from("builds")])
    };
    let body = vivi_mail::letter::fixture::need_body_with(&front, "Build the thing");
    let thread = json!([{
        "handle": NEED,
        "message_id": "m1",
        "from": "operator@local",
        "subject": "Need",
        "body": body,
    }]);
    rig.host.reply("need show", 0, &thread.to_string());
}

fn rig_branch(cfg: PumpConfig, row: Option<NeedBranchRow>) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let mains: Vec<(String, String)> = cfg
        .target_configs()
        .into_values()
        .map(|target| (target.remote, target.main))
        .collect();
    let (ctx, host) = test_ctx(dir.path(), cfg);
    // Every fixture can read the current tip of each target's main; only
    // whether the need branch is published and holds work varies.
    for (remote, main) in mains {
        host.reply(
            &format!("ls-remote {remote} refs/heads/{main}"),
            0,
            &format!("{MAIN}\trefs/heads/{main}\n"),
        );
    }
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    if let Some(row) = row {
        store.upsert_need_branch(&row)?;
    }
    Ok(Rig {
        _dir: dir,
        ctx,
        host,
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
    /// Work-bearing fixture: an admitted plan, one merged slice, an audit with
    /// a finding and a gap, a judge reason and a failing need rung. The merged
    /// slice is published on the need branch above the target's main, so the
    /// slicer's base is that tip.
    fn seed(&self) -> R {
        self.seed_findings()?;
        self.store
            .insert_step(NEED, &step("slice:s1", "t-s1", Some("merged")))?;
        self.store
            .insert_step(NEED, &step("slice:s2", "t-s2", Some("failed")))?;
        publish_tip(self, TIP);
        tip_outside_main(self);
        Ok(())
    }

    /// Empty fixture: the same findings with no merged slice and no published
    /// need-branch commit, so the base is the target's current main.
    fn seed_empty(&self) -> R {
        self.seed_findings()
    }

    /// The plan, audit, judge and failing rung rows every rework brief reads.
    fn seed_findings(&self) -> R {
        let rows = [
            ("plan", "planner", "t-plan", Some("planned")),
            ("audit1", "auditor", "t-audit", Some("block")),
            ("judge1", "judge", "t-judge", Some("block")),
            ("test1", "rung-need", "t-rung", Some("fail")),
        ];
        for (node, _, task, verdict) in rows {
            self.store.insert_step(NEED, &step(node, task, verdict))?;
        }
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
    // The published need branch tip holds the merged slice.
    assert_eq!(job.from_ref.as_deref(), Some(TIP));
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
    assert!(body.starts_with("Return rework slices"), "{body}");
    assert!(body.contains("read-only"), "{body}");
    assert!(body.contains("mode: rework\n"), "{body}");
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
    assert_slicer_footer(&body, SlicerMode::Rework, "default");
    assert!(
        body.find("new_fact: X-FACT").unwrap()
            < body.find("The actual reply starts immediately").unwrap(),
        "footer precedes the CTO ruling: {body}"
    );

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

/// What the shipped registry's handler answers for rework1.
fn rework_start(rig: &Rig) -> Result<StartAction, VivariumError> {
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
    handler.start(&cx, &step("rework1", "t-rw", None))
}

/// The rework1 job the shipped registry starts, for budget assertions.
fn shipped_rework_job(rig: &Rig) -> Result<Box<JobSpec>, VivariumError> {
    match rework_start(rig)? {
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
    Ok(shipped_rework_job(rig)?.body.clone())
}

fn assert_slicer_footer(body: &str, mode: SlicerMode, target: &str) {
    let footer = crate::pump::envelope::format::slicer(mode, target, "slice-n1-example");
    assert!(body.ends_with(&footer), "missing complete footer: {body}");
    let parsed: Vec<_> = envelopes(&footer)
        .iter()
        .filter_map(|example| parse_role_reply::<Slicer>(example).ok())
        .collect();
    assert_eq!(parsed.len(), 2, "{footer}");
    let sliced = &parsed[0].front;
    assert_eq!(sliced.answer, SlicerAnswer::Sliced);
    assert_eq!(sliced.mode, mode);
    assert_eq!(sliced.slice.len(), 1);
    assert_eq!(sliced.slice[0].id, "slice-n1-example");
    assert_eq!(sliced.slice[0].target, target);
    assert!(!sliced.slice[0].title.trim().is_empty());
    assert!(!sliced.slice[0].brief.trim().is_empty());
    assert!(!sliced.slice[0].write_scope.is_empty());
    assert!(sliced.slice[0].deps.is_empty());
    assert_eq!(parsed[1].front.answer, SlicerAnswer::Escalate);
    assert_eq!(parsed[1].front.mode, mode);
}

fn envelopes(text: &str) -> Vec<String> {
    let mut inside = false;
    let mut current = String::new();
    let mut found = Vec::new();
    for line in text.lines() {
        if line == "+++" {
            if inside {
                found.push(format!("+++\n{}\n+++", current.trim_end()));
                current.clear();
            }
            inside = !inside;
        } else if inside {
            current.push_str(line);
            current.push('\n');
        }
    }
    found
}

/// The target `sandbox` beside the primary `default`: a job bound to the need
/// branch's own target, not the primary one.
fn two_targets() -> PumpConfig {
    let mut cfg = PumpConfig::default();
    cfg.targets
        .insert(String::from("default"), PumpTarget::default());
    cfg.targets.insert(
        String::from("sandbox"),
        PumpTarget {
            path: String::from("pump-sandbox"),
            main: String::from("vivi/main"),
            ..PumpTarget::default()
        },
    );
    cfg
}

/// The need branch is published: the job starts from its tip, which holds the
/// merged work, and not from main or the recorded base. The brief names the
/// same selection, and the merged slices stay in context.
#[test]
fn a_rework_slicer_starts_from_the_published_need_branch_tip() -> R {
    let rig = rig()?;
    rig.seed()?;

    let job = shipped_rework_job(&rig)?;

    assert_eq!(job.target.as_deref(), Some("default"));
    assert_eq!(job.from_ref.as_deref(), Some(TIP));
    assert_ne!(job.from_ref.as_deref(), Some(MAIN));
    assert_ne!(job.from_ref.as_deref(), Some(BASE));
    assert!(job.read_only);
    assert_eq!(
        job.extra_targets,
        crate::pump::flow::step_inputs::targets(&rig.ctx)
    );
    // The brief names the very snapshot the job checks out.
    assert!(
        job.body.contains("base source: need branch"),
        "{}",
        job.body
    );
    assert!(
        job.body.contains("base ref: refs/heads/vivi/need/n1"),
        "{}",
        job.body
    );
    assert!(
        job.body.contains(&format!("base sha: {TIP}")),
        "{}",
        job.body
    );
    assert!(
        job.body.contains(&format!(
            "target: {}",
            job.target.as_deref().unwrap_or_default()
        )),
        "{}",
        job.body
    );
    assert!(
        !job.body.contains("base fallback:"),
        "a need-branch base needs no fallback: {}",
        job.body
    );
    // Merged slices stay in the brief: the slicer must not re-cut them.
    assert!(job.body.contains("- slice:s1 (task t-s1)"), "{}", job.body);
    Ok(())
}

/// The need branch row records the target of the need's first slice; every
/// later read follows that target, not the dispatcher's primary one.
#[test]
fn a_rework_slicer_follows_the_target_stored_on_the_need_branch() -> R {
    let row = NeedBranchRow {
        target: String::from("sandbox"),
        ..live_row()
    };
    let rig = rig_branch(two_targets(), Some(row))?;
    rig.seed()?;

    let job = shipped_rework_job(&rig)?;

    assert_eq!(job.target.as_deref(), Some("sandbox"));
    assert_eq!(job.from_ref.as_deref(), Some(TIP));
    assert!(job.read_only);
    assert_slicer_footer(&job.body, SlicerMode::Rework, "sandbox");
    assert!(
        job.extra_targets.contains(&String::from("default")),
        "{:?}",
        job.extra_targets
    );
    assert!(
        job.extra_targets.contains(&String::from("sandbox")),
        "{:?}",
        job.extra_targets
    );
    Ok(())
}

/// The brief and the job name the current-main selection, and the brief
/// carries why main was chosen.
fn assert_main_base(job: &JobSpec) {
    assert_eq!(job.from_ref.as_deref(), Some(MAIN));
    assert!(
        job.body.contains(&format!("base sha: {MAIN}")),
        "{}",
        job.body
    );
    assert!(
        job.body.contains("base source: target main"),
        "{}",
        job.body
    );
    assert!(
        job.body.contains("base ref: refs/heads/main"),
        "{}",
        job.body
    );
    assert!(
        job.body.contains("base fallback: "),
        "a main base carries its reason: {}",
        job.body
    );
}

/// A live row whose branch was never published records only an old base:
/// rework still dispatches, from the target's current main, and the row is
/// left exactly as it was found.
#[test]
fn an_unpublished_need_branch_starts_rework_from_the_current_main() -> R {
    let rig = rig()?;
    rig.seed_empty()?;

    let job = shipped_rework_job(&rig)?;

    assert_main_base(&job);
    assert_ne!(job.from_ref.as_deref(), Some(BASE));
    assert!(
        job.body
            .contains("base fallback: vivi/need/n1 is not published"),
        "{}",
        job.body
    );
    let row = rig
        .store
        .need_branch(NEED)?
        .ok_or_else(|| VivariumError::Other("the need branch row vanished".into()))?;
    assert_eq!(row.base.as_deref(), Some(BASE));
    assert_eq!(row.status, "live");
    assert_eq!(row.run, 1);
    assert_eq!(row.branch, "vivi/need/n1");
    Ok(())
}

/// An unpublished live row without even a recorded base: fresh state, so
/// rework dispatches from the current main.
#[test]
fn an_unpublished_live_row_without_a_base_starts_rework_from_the_current_main() -> R {
    let row = NeedBranchRow {
        base: None,
        ..live_row()
    };
    let rig = rig_branch(PumpConfig::default(), Some(row))?;
    rig.seed_empty()?;

    let job = shipped_rework_job(&rig)?;

    assert_main_base(&job);
    let row = rig
        .store
        .need_branch(NEED)?
        .ok_or_else(|| VivariumError::Other("the need branch row vanished".into()))?;
    assert_eq!(row.base, None);
    assert_eq!(row.status, "live");
    Ok(())
}

/// No need branch row at all: the base is the target's main, and no row is
/// fabricated for it.
#[test]
fn a_missing_need_branch_row_starts_rework_from_the_current_main() -> R {
    let rig = rig_branch(PumpConfig::default(), None)?;
    rig.seed_empty()?;
    root_need(&rig, Some("default"));

    let job = shipped_rework_job(&rig)?;

    assert_main_base(&job);
    assert_eq!(job.target.as_deref(), Some("default"));
    assert!(
        job.body
            .contains("base fallback: the need has no need branch row"),
        "{}",
        job.body
    );
    assert!(
        rig.store.need_branch(NEED)?.is_none(),
        "starting rework fabricated a need branch row"
    );
    Ok(())
}

/// A pruned or fresh row falls back too: nothing was published, so the
/// current main is the snapshot, and the row keeps its state.
#[test]
fn a_fresh_need_branch_row_starts_rework_from_the_current_main() -> R {
    let row = NeedBranchRow {
        status: String::from("fresh"),
        ..live_row()
    };
    let rig = rig_branch(PumpConfig::default(), Some(row))?;
    rig.seed_empty()?;

    let job = shipped_rework_job(&rig)?;

    assert_main_base(&job);
    assert!(
        job.body
            .contains("base fallback: the need branch row is fresh"),
        "{}",
        job.body
    );
    let row = rig
        .store
        .need_branch(NEED)?
        .ok_or_else(|| VivariumError::Other("the need branch row vanished".into()))?;
    assert_eq!(row.status, "fresh");
    assert_eq!(row.base.as_deref(), Some(BASE));
    Ok(())
}

/// A row whose branch is still being moved is not a base: rework refuses, and
/// the row is left alone.
#[test]
fn a_need_branch_that_is_being_moved_starts_no_rework_job() -> R {
    for status in ["renaming", "deleting"] {
        let row = NeedBranchRow {
            status: String::from(status),
            ..live_row()
        };
        let rig = rig_branch(PumpConfig::default(), Some(row))?;
        rig.seed_empty()?;

        let err = rework_start(&rig)
            .err()
            .ok_or_else(|| VivariumError::Other(format!("a {status} row started a job")))?;
        assert!(matches!(err, VivariumError::Other(_)), "{err}");
        assert!(err.to_string().contains(status), "{err}");
        let row = rig
            .store
            .need_branch(NEED)?
            .ok_or_else(|| VivariumError::Other("the need branch row vanished".into()))?;
        assert_eq!(row.status, status);
        assert_eq!(row.base.as_deref(), Some(BASE));
    }
    Ok(())
}

/// A run that enters at rework1 with nothing recorded and no need branch row
/// still dispatches: the base is the target's main, named by the root need.
#[test]
fn a_rework_entry_without_history_starts_from_the_current_main() -> R {
    let mut rig = rig_branch(PumpConfig::default(), None)?;
    rig.run.entry = String::from("rework1");
    root_need(&rig, Some("default"));

    let job = shipped_rework_job(&rig)?;

    assert_main_base(&job);
    assert_eq!(job.target.as_deref(), Some("default"));
    assert!(
        job.body.contains("(no admitted plan recorded)"),
        "{}",
        job.body
    );
    assert!(job.body.contains("(no audit recorded)"), "{}", job.body);
    assert!(job.body.contains("## CTO ruling"), "{}", job.body);
    assert!(
        job.body.contains("(no CTO ruling; an operator unlock"),
        "{}",
        job.body
    );
    Ok(())
}

/// The brief and the job share one immutable selection. Ancestry adds its own
/// Git queries; what matters is that the branch is never resolved twice.
#[test]
fn one_base_selection_supplies_the_job_and_the_brief() -> R {
    let rig = rig()?;
    rig.seed()?;

    let job = shipped_rework_job(&rig)?;

    let branch_reads = rig
        .host
        .commands()
        .iter()
        .filter(|command| command.contains("ls-remote") && command.contains("vivi/need/"))
        .count();
    assert_eq!(
        branch_reads, 1,
        "the need branch was resolved more than once"
    );
    assert_eq!(job.from_ref.as_deref(), Some(TIP));
    assert!(
        job.body
            .contains("base sha: 89abcdef0123456789abcdef0123456789abcdef"),
        "{}",
        job.body
    );
    assert!(
        job.body.contains("base ref: refs/heads/vivi/need/n1"),
        "{}",
        job.body
    );
    assert!(
        job.body.contains("base source: need branch"),
        "{}",
        job.body
    );
    assert_eq!(job.target.as_deref(), Some("default"));
    Ok(())
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
