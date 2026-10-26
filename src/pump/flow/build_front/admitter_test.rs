//! Tests for the admitter step handler.

use std::collections::BTreeMap;

use tempfile::{TempDir, tempdir};

use crate::error::VivariumError;
use crate::mailspace::shipped::ADMITTER_ROLE;
use crate::pump::config::PumpConfig;
use crate::pump::flow::def::{FlowDef, FlowKind};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::{Registry, shipped};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::need_scope::{RulingDecision, ScopeIssue, ScopeRuling, ruling_note_body};
use crate::pump::flow::outcome::{StartAction, StartCx};
use crate::pump::flow::step_inputs::POOL_RESULT_MARKER;
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::jobs::{JobSpec, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;
type Reg = Registry<Jobs, FakeMail>;

const NEED: &str = "n1";

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

struct Rig {
    _dir: TempDir,
    ctx: Ctx,
    store: PumpStore,
    mail: FakeMail,
    run: RunRow,
    def: FlowDef,
}

fn rig() -> Result<Rig, VivariumError> {
    rig_with(PumpConfig::default())
}

fn rig_with(cfg: PumpConfig) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, host) = test_ctx(dir.path(), cfg);
    host.reply("need show", 0, "the need thread");
    let mut routing = (*ctx.routing).clone();
    routing.roles.push(RoutedRole {
        name: String::from(ADMITTER_ROLE),
        kind: Some(String::from("admitter")),
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

    /// A Done planner step `node` whose task holds one pool result with `plan`.
    fn planned(&mut self, node: &str, plan: &str) -> R {
        let task = self.mail.send_step("pump", "pump", node, "body", &[])?;
        self.mail.reply(
            &task,
            &format!(
                "{POOL_RESULT_MARKER}j: finished\npool p  slot 1  model m\n\n--- receipt\n{plan}"
            ),
        )?;
        self.done_row(node, &task)
    }

    fn done_row(&self, node: &str, task: &str) -> R {
        self.store.insert_step(
            NEED,
            &StepRow {
                run: 1,
                node: node.into(),
                task: task.into(),
                state: StepState::Done,
                verdict: None,
            },
        )
    }

    fn start(&self, node: &str) -> Result<StartAction, VivariumError> {
        let reg: Reg = shipped(&self.ctx);
        let flow_node = self
            .def
            .nodes
            .iter()
            .find(|n| n.id == node)
            .ok_or_else(|| VivariumError::Message(format!("no node {node}")))?;
        let handler = reg
            .handler_for(FlowKind::Build, flow_node)
            .ok_or_else(|| VivariumError::Message(format!("no handler for {node}")))?;
        let step = StepRow {
            run: 1,
            node: node.into(),
            task: format!("task-{node}"),
            state: StepState::Running,
            verdict: None,
        };
        let cx = StartCx {
            ctx: &self.ctx,
            store: &self.store,
            mail: &self.mail,
            run: &self.run,
            def: &self.def,
        };
        handler.start(&cx, &step)
    }
}

fn job(action: StartAction) -> Result<JobSpec, VivariumError> {
    match action {
        StartAction::Job(spec) => Ok(*spec),
        other => Err(VivariumError::Message(format!("not a job: {other:?}"))),
    }
}

#[test]
fn admit1_is_a_read_only_admitter_job_carrying_the_plan_and_thread() -> R {
    let mut rig = rig()?;
    rig.planned("plan", "PLAN ONE TEXT")?;

    let spec = job(rig.start("admit1")?)?;

    assert_eq!(spec.role, ADMITTER_ROLE);
    assert!(spec.read_only);
    assert!(spec.body.contains("PLAN ONE TEXT"), "{}", spec.body);
    assert!(spec.body.contains("the need thread"), "{}", spec.body);
    assert!(spec.body.contains("need: n1"), "{}", spec.body);
    assert!(spec.body.contains("run: 1"), "{}", spec.body);
    Ok(())
}

#[test]
fn admit2_reads_plan2_not_plan() -> R {
    let mut rig = rig()?;
    rig.planned("plan", "PLAN ONE TEXT")?;
    rig.planned("plan2", "PLAN TWO TEXT")?;

    let spec = job(rig.start("admit2")?)?;

    assert!(spec.body.contains("PLAN TWO TEXT"), "{}", spec.body);
    assert!(!spec.body.contains("PLAN ONE TEXT"), "{}", spec.body);
    Ok(())
}

#[test]
fn a_missing_plan_reply_is_an_error_naming_the_node() -> R {
    let mut rig = rig()?;
    let task = rig.mail.send_step("pump", "pump", "plan", "body", &[])?;
    rig.done_row("plan", &task)?;

    let err = rig.start("admit1").unwrap_err();

    assert!(err.to_string().contains("admit1"), "{err}");
    Ok(())
}

#[test]
fn no_finished_planner_step_is_an_error() -> R {
    let rig = rig()?;

    let err = rig.start("admit1").unwrap_err();

    assert!(err.to_string().contains("admit1"), "{err}");
    Ok(())
}

#[test]
fn admit1_and_admit2_budgets_follow_a_nondefault_dispatch_fallback() -> R {
    let mut cfg = PumpConfig::default();
    cfg.dispatch.budget = 222;
    let mut rig = rig_with(cfg)?;
    rig.planned("plan", "PLAN ONE TEXT")?;
    rig.planned("plan2", "PLAN TWO TEXT")?;

    let admit1 = job(rig.start("admit1")?)?;
    let admit2 = job(rig.start("admit2")?)?;

    // No `[pump.dispatch.step_budgets]` key: both admitter nodes take the
    // changed shared fallback, exactly.
    assert_eq!(admit1.budget, 222);
    assert_eq!(admit2.budget, 222);
    for spec in [&admit1, &admit2] {
        assert!(spec.read_only);
        assert_eq!(spec.from_ref.as_deref(), Some(rig.ctx.cfg.main.as_str()));
        assert_eq!(spec.extra_targets, vec![String::from("default")]);
    }
    assert!(admit1.body.contains("need: n1"), "{}", admit1.body);
    Ok(())
}

#[test]
fn admit1_and_admit2_budgets_take_the_admitter_override_not_another_kind() -> R {
    let mut cfg = PumpConfig::default();
    cfg.dispatch.budget = 222;
    cfg.dispatch.step_budgets.admitter = Some(333);
    // Another kind configured: it must not reach the admitter nodes.
    cfg.dispatch.step_budgets.planner = Some(444);
    let mut rig = rig_with(cfg)?;
    rig.planned("plan", "PLAN ONE TEXT")?;
    rig.planned("plan2", "PLAN TWO TEXT")?;

    let admit1 = job(rig.start("admit1")?)?;
    let admit2 = job(rig.start("admit2")?)?;

    assert_eq!(admit1.budget, 333);
    assert_eq!(admit2.budget, 333);
    assert!(admit1.read_only);
    assert_eq!(admit1.from_ref.as_deref(), Some(rig.ctx.cfg.main.as_str()));
    assert_eq!(admit1.extra_targets, vec![String::from("default")]);
    Ok(())
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
fn admit_brief_groups_effective_scope_outside_the_thread() -> R {
    let mut rig = rig()?;
    rig.delegate(&["src/pump/"]);
    rig.planned("plan", "PLAN ONE TEXT")?;
    from_reply_role(
        &mut rig,
        NEED,
        &a_grant(NEED, "default", &["docs/scope.md"]),
    )?;

    let spec = job(rig.start("admit1")?)?;

    // The stale fixture thread cannot carry the grant; the authoritative
    // block before the plan carries delegation and grant, grouped.
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
    let thread_at = spec
        .body
        .find("\nthread:\nthe need thread")
        .ok_or_else(|| VivariumError::Other(String::from("no thread")))?;
    assert!(scope_at < thread_at);
    Ok(())
}

#[test]
fn admit_brief_keeps_foreign_and_operator_notes_off_the_scope() -> R {
    let mut rig = rig()?;
    rig.delegate(&["src/pump/"]);
    rig.planned("plan", "PLAN ONE TEXT")?;
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

    let spec = job(rig.start("admit1")?)?;

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
fn an_empty_original_scope_is_not_unrestricted_permission_in_the_admit_brief() -> R {
    let mut rig = rig()?;
    // Even a reply-role grant cannot widen an empty delegation.
    from_reply_role(
        &mut rig,
        NEED,
        &a_grant(NEED, "default", &["docs/scope.md"]),
    )?;
    rig.planned("plan", "PLAN ONE TEXT")?;

    let spec = job(rig.start("admit1")?)?;

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
fn admit_brief_states_the_escalate_request_contract() -> R {
    let mut rig = rig()?;
    rig.planned("plan", "PLAN ONE TEXT")?;

    let spec = job(rig.start("admit1")?)?;

    assert!(
        spec.body
            .contains("scope request contract (independent of installed charters)"),
        "{}",
        spec.body
    );
    assert!(
        spec.body
            .contains("answers `escalate` with nonblank `scope_target`, nonempty `scope_paths`, and a `reason`"),
        "{}",
        spec.body
    );
    assert!(
        spec.body
            .contains("a general operator or design question omits the scope fields"),
        "{}",
        spec.body
    );
    assert!(
        spec.body
            .contains("target policy may grant additions automatically; it never grants concurrent slice ownership"),
        "{}",
        spec.body
    );
    Ok(())
}
