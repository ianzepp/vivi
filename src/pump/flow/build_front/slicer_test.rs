//! Tests for the slicer step handler (node `slice`).

use tempfile::{TempDir, tempdir};

use super::SliceStep;
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::flow::def::{FlowDef, FlowKind};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::{Registry, shipped};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::need_scope::{RulingDecision, ScopeIssue, ScopeRuling, ruling_note_body};
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::step_inputs::POOL_RESULT_MARKER;
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::jobs::JobSpec;
use crate::pump::state::State;
use crate::pump::store::{DB_FILENAME, PumpStore};
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

const NEED: &str = "n1";

struct Rig {
    _dir: TempDir,
    ctx: Ctx,
    store: PumpStore,
    mail: FakeMail,
    run: RunRow,
    def: FlowDef,
}

fn rig_with(cfg: PumpConfig) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, _host) = test_ctx(dir.path(), cfg);
    let mut routing = (*ctx.routing).clone();
    routing.roles.push(RoutedRole {
        name: String::from("slicer"),
        kind: Some(String::from("slicer")),
        status: String::from("active"),
        pool: Some(String::from("test-planner-pool")),
        read_only: Some(true),
        model: None,
        models: Vec::new(),
    });
    let ctx = ctx.with_routing(routing);
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let run = RunRow {
        need: NEED.into(),
        run: 1,
        flow: "build".into(),
        flow_version: 2,
        kind: "build".into(),
        entry: "plan".into(),
        state: "live".into(),
    };
    store.insert_run(&run)?;
    let mut mail = FakeMail::new("pump");
    let body = crate::letter::fixture::need_body(&["build the widget"], "prose");
    let sent = mail.send_step("pump", "pump", "the need", &body, &[])?;
    if let Some(task) = mail.tasks.remove(&sent) {
        mail.tasks.insert(NEED.to_string(), task);
    }
    Ok(Rig {
        _dir: dir,
        ctx,
        store,
        mail,
        run,
        def: load(FlowKind::Build)?,
    })
}

fn rig() -> Result<Rig, VivariumError> {
    rig_with(PumpConfig::default())
}

fn row(node: &str, task: &str, state: StepState) -> StepRow {
    StepRow {
        run: 1,
        node: node.into(),
        task: task.into(),
        state,
        verdict: None,
    }
}

/// A Done planner step `node` whose task holds one pool result with `plan`.
fn planner_done(rig: &mut Rig, node: &str, plan: Option<&str>) -> R {
    let task = rig.mail.send_step("pump", "pump", node, "body", &[])?;
    if let Some(plan) = plan {
        let text = format!(
            "{POOL_RESULT_MARKER}j-{node}: finished\npool p  slot 1  model m\n\n--- receipt\n{plan}"
        );
        rig.mail.reply(&task, &text)?;
    }
    rig.store
        .insert_step(NEED, &row(node, &task, StepState::Done))
}

fn start(rig: &Rig) -> Result<JobSpec, VivariumError> {
    let cx = StartCx {
        ctx: &rig.ctx,
        store: &rig.store,
        mail: &rig.mail,
        run: &rig.run,
        def: &rig.def,
    };
    match SliceStep.start(&cx, &row("slice", "ts", StepState::Waiting))? {
        StartAction::Job(spec) => Ok(*spec),
        other => Err(VivariumError::Other(format!("not a job: {other:?}"))),
    }
}

#[test]
fn start_is_a_read_only_slicer_job_with_the_plan_and_default_minutes() -> R {
    let mut rig = rig()?;
    planner_done(&mut rig, "plan", Some("PLAN-ONE"))?;

    let spec = start(&rig)?;

    assert_eq!(spec.role, "slicer");
    assert!(spec.read_only);
    assert_eq!(spec.task.as_deref(), Some("ts"));
    assert!(spec.body.starts_with("mode: slice\n"), "{}", spec.body);
    assert!(spec.body.contains("need: n1\nrun: 1\n"), "{}", spec.body);
    assert!(spec.body.contains("targets: default"), "{}", spec.body);
    assert!(
        spec.body
            .contains("slice size: aim for 5 to 10 minutes per slice"),
        "{}",
        spec.body
    );
    assert!(
        spec.body.contains("admitted plan:\nPLAN-ONE"),
        "{}",
        spec.body
    );
    Ok(())
}

#[test]
fn configured_minutes_set_the_slice_size() -> R {
    let mut cfg = PumpConfig::default();
    cfg.slice.minutes = Some(6);
    let mut rig = rig_with(cfg)?;
    planner_done(&mut rig, "plan", Some("PLAN-ONE"))?;

    let spec = start(&rig)?;

    assert!(
        spec.body
            .contains("slice size: aim for 5 to 6 minutes per slice"),
        "{}",
        spec.body
    );
    Ok(())
}

#[test]
fn slice_budgets_follow_a_nondefault_dispatch_fallback() -> R {
    let mut cfg = PumpConfig::default();
    cfg.dispatch.budget = 222;
    let mut rig = rig_with(cfg)?;
    planner_done(&mut rig, "plan", Some("PLAN-ONE"))?;

    let spec = start(&rig)?;

    // No `[pump.dispatch.step_budgets]` key: the slice node takes the changed
    // shared fallback, exactly.
    assert_eq!(spec.budget, 222);
    assert!(spec.read_only);
    assert_eq!(spec.from_ref.as_deref(), Some(rig.ctx.cfg.main.as_str()));
    assert_eq!(spec.extra_targets, vec![String::from("default")]);
    // The sizing guidance stays the ten-minute default; the budget is not a
    // sizing input and the minutes are not a hard stop.
    assert_eq!(rig.ctx.cfg.slice.minutes(), 10);
    assert!(
        spec.body
            .contains("slice size: aim for 5 to 10 minutes per slice"),
        "{}",
        spec.body
    );
    Ok(())
}

#[test]
fn slice_budget_overrides_do_not_touch_the_slice_size_guidance() -> R {
    let mut cfg = PumpConfig::default();
    cfg.dispatch.budget = 222;
    cfg.dispatch.step_budgets.slicer = Some(77);
    // Other kinds configured: none of them may reach the slice node.
    cfg.dispatch.step_budgets.judge = Some(88);
    cfg.dispatch.step_budgets.planner = Some(66);
    let mut rig = rig_with(cfg)?;
    planner_done(&mut rig, "plan", Some("PLAN-ONE"))?;

    let spec = start(&rig)?;

    assert_eq!(spec.budget, 77);
    assert!(spec.read_only);
    assert_eq!(spec.from_ref.as_deref(), Some(rig.ctx.cfg.main.as_str()));
    assert_eq!(spec.extra_targets, vec![String::from("default")]);
    // Execution budgets never move `SliceConfig::minutes()` or the brief's
    // default "aim for 5 to 10 minutes" guidance.
    assert_eq!(rig.ctx.cfg.slice.minutes(), 10);
    assert!(
        spec.body
            .contains("slice size: aim for 5 to 10 minutes per slice"),
        "{}",
        spec.body
    );
    Ok(())
}

#[test]
fn the_revised_plan_wins_over_the_first() -> R {
    let mut rig = rig()?;
    planner_done(&mut rig, "plan", Some("PLAN-ONE"))?;
    planner_done(&mut rig, "plan2", Some("PLAN-TWO"))?;

    let spec = start(&rig)?;

    assert!(spec.body.contains("PLAN-TWO"), "{}", spec.body);
    assert!(!spec.body.contains("PLAN-ONE"), "{}", spec.body);
    Ok(())
}

#[test]
fn no_done_planner_step_is_an_error() -> R {
    let rig = rig()?;

    let err = start(&rig).unwrap_err();

    assert!(err.to_string().contains(NEED), "{err}");
    Ok(())
}

#[test]
fn a_planner_step_without_a_reply_is_an_error() -> R {
    let mut rig = rig()?;
    planner_done(&mut rig, "plan", None)?;

    let err = start(&rig).unwrap_err();

    assert!(err.to_string().contains("no reply"), "{err}");
    Ok(())
}

#[test]
fn shipped_registers_slice_and_the_rework_step_override_for_rework1() -> R {
    let rig = rig()?;
    let registry: Registry<State, FakeMail> = shipped(&rig.ctx);
    let node = |id: &str| {
        rig.def
            .nodes
            .iter()
            .find(|n| n.id == id)
            .ok_or_else(|| VivariumError::Other(format!("no node {id}")))
    };

    assert!(
        registry
            .handler_for(FlowKind::Build, node("slice")?)
            .is_some()
    );
    // rework1 has N5's rework_step node override, not this slicer's handler.
    assert!(
        registry
            .handler_for(FlowKind::Build, node("rework1")?)
            .is_some()
    );
    Ok(())
}

impl Rig {
    /// Rewrites the rig need's letter to delegate `scope` as its write scope.
    fn delegate(&mut self, scope: &[&str]) {
        let mut front = crate::letter::work::NeedFront::new(vec![String::from("build the widget")]);
        front.write_scope = scope.iter().map(|path| (*path).to_string()).collect();
        let body = crate::letter::fixture::need_body_with(&front, "prose");
        if let Some(task) = self.mail.tasks.get_mut(NEED) {
            task.body = body;
        }
    }
}

/// A grant ruling for `need`.
fn a_grant(need: &str, target: &str, paths: &[&str]) -> ScopeRuling {
    ScopeRuling {
        need: need.to_string(),
        target: target.to_string(),
        run: 1,
        task: "t0009".to_string(),
        paths: paths.iter().map(|path| (*path).to_string()).collect(),
        reason: "held by no open slice".to_string(),
        decision: RulingDecision::Grant,
        issue: None,
    }
}

/// An operator-required ruling for `need`: it grants nothing.
fn an_operator_ruling(need: &str, target: &str, paths: &[&str]) -> ScopeRuling {
    ScopeRuling {
        decision: RulingDecision::OperatorRequired,
        issue: Some(ScopeIssue::Denied),
        ..a_grant(need, target, paths)
    }
}

/// Post `ruling` on `need` as the pump's reply role (`[pump] reply_from`).
fn from_reply_role(rig: &mut Rig, need: &str, ruling: &ScopeRuling) -> R {
    let body = ruling_note_body(ruling, "job-1")?;
    rig.mail.foreign_reply(need, "mind", &body);
    Ok(())
}

/// Post the note body for `ruling` on `need` as a sender other than the
/// pump's reply role.
fn from_other_sender(rig: &mut Rig, need: &str, ruling: &ScopeRuling) -> R {
    let body = ruling_note_body(ruling, "job-2")?;
    rig.mail.foreign_reply(need, "planner", &body);
    Ok(())
}

#[test]
fn slice_brief_groups_effective_scope_before_the_plan() -> R {
    let mut rig = rig()?;
    rig.delegate(&["src/pump/"]);
    planner_done(&mut rig, "plan", Some("PLAN-ONE"))?;
    from_reply_role(
        &mut rig,
        NEED,
        &a_grant(NEED, "default", &["docs/scope.md"]),
    )?;

    let spec = start(&rig)?;

    assert!(
        spec.body
            .contains("effective need write scope (grouped by target; the original letter's delegation plus every later pump grant; authoritative for this job and supersedes the original letter's write_scope):"),
        "{}",
        spec.body
    );
    assert!(
        spec.body
            .contains("- target default: delegated src/pump/; granted docs/scope.md"),
        "{}",
        spec.body
    );
    let scope_at = spec
        .body
        .find("effective need write scope")
        .ok_or_else(|| VivariumError::Other(String::from("no scope block")))?;
    let plan_at = spec
        .body
        .find("\nadmitted plan:\n")
        .ok_or_else(|| VivariumError::Other(String::from("no plan")))?;
    assert!(scope_at < plan_at);
    Ok(())
}

#[test]
fn slice_brief_keeps_foreign_and_operator_notes_off_the_scope() -> R {
    let mut rig = rig()?;
    rig.delegate(&["src/pump/"]);
    planner_done(&mut rig, "plan", Some("PLAN-ONE"))?;
    from_reply_role(
        &mut rig,
        NEED,
        &a_grant(NEED, "default", &["docs/scope.md"]),
    )?;
    from_other_sender(
        &mut rig,
        NEED,
        &a_grant(NEED, "default", &["docs/stray.md"]),
    )?;
    from_reply_role(
        &mut rig,
        NEED,
        &an_operator_ruling(NEED, "default", &["docs/denied.md"]),
    )?;

    let spec = start(&rig)?;

    assert!(
        spec.body
            .contains("- target default: delegated src/pump/; granted docs/scope.md"),
        "{}",
        spec.body
    );
    assert!(!spec.body.contains("docs/stray.md"), "{}", spec.body);
    assert!(!spec.body.contains("docs/denied.md"), "{}", spec.body);
    Ok(())
}

#[test]
fn an_empty_original_scope_is_not_unrestricted_permission_in_the_slice_brief() -> R {
    let mut rig = rig()?;
    // Even a reply-role grant cannot widen an empty delegation.
    from_reply_role(
        &mut rig,
        NEED,
        &a_grant(NEED, "default", &["docs/scope.md"]),
    )?;
    planner_done(&mut rig, "plan", Some("PLAN-ONE"))?;

    let spec = start(&rig)?;

    assert!(
        spec.body.contains(
            "- (none): the need delegates no write scope; that is not unrestricted permission"
        ),
        "{}",
        spec.body
    );
    assert!(!spec.body.contains("docs/scope.md"), "{}", spec.body);
    Ok(())
}

#[test]
fn the_slice_brief_carries_no_request_contract() -> R {
    let mut rig = rig()?;
    planner_done(&mut rig, "plan", Some("PLAN-ONE"))?;

    let spec = start(&rig)?;

    // Request contracts live in the planner and admitter briefs only; the
    // slicer names no scope-request channel.
    assert!(
        !spec.body.contains("scope request contract"),
        "{}",
        spec.body
    );
    Ok(())
}
