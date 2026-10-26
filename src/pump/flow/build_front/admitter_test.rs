//! Tests for the admitter step handler.

use std::collections::BTreeMap;

use tempfile::{TempDir, tempdir};

use crate::error::VivariumError;
use crate::mailspace::shipped::ADMITTER_ROLE;
use crate::pump::config::PumpConfig;
use crate::pump::config::PumpTarget;
use crate::pump::config::scope::ScopePolicy;
use crate::pump::flow::def::{FlowDef, FlowKind};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::{Registry, shipped};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::outcome::{StartAction, StartCx};
use crate::pump::flow::step_inputs::POOL_RESULT_MARKER;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobSpec, JobStore, PoolJob};
use crate::pump::store::scope_ruling::{RulingDecision, ScopeRulingRequest};
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
    host: std::sync::Arc<FakeHost>,
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
    // A valid immutable need root under the need handle, so the brief's
    // authoritative scope section reads the root letter like production does.
    let mut mail = FakeMail::new("pump");
    let body = crate::letter::fixture::need_body(&["build the widget"], "prose");
    let sent = mail.send_step("pump", "pump", "the need", &body, &[])?;
    if let Some(task) = mail.tasks.remove(&sent) {
        mail.tasks.insert(NEED.to_string(), task);
    }
    Ok(Rig {
        _dir: dir,
        ctx,
        host,
        store,
        mail,
        run,
        def: load(FlowKind::Build)?,
    })
}

impl Rig {
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

/// Store one scope ruling for `NEED` on `target`.
fn ruling(store: &PumpStore, job: &str, target: &str, paths: &[&str], decision: RulingDecision) {
    store
        .insert_scope_ruling(&ScopeRulingRequest {
            job: job.into(),
            job_kind: "hand".into(),
            need: NEED.into(),
            run: 1,
            node: "slice:n1-1".into(),
            task: format!("task-{job}"),
            role: "hand".into(),
            target: target.into(),
            paths: paths.iter().map(|path| (*path).to_string()).collect(),
            reason: "the slice needs the path to land".into(),
            decision,
            policy_reason: None,
            policy: ScopePolicy::snapshot(target, &["./".into()], &[]).expect("policy snapshot"),
        })
        .expect("ruling stored");
}

/// A need root under `NEED` with `write_scope` and an optional named target.
fn seed_need(rig: &mut Rig, write_scope: &[&str], target: Option<&str>) -> R {
    let mut front = crate::letter::work::NeedFront::new(vec![String::from("build the widget")]);
    front.write_scope = write_scope.iter().map(|path| (*path).to_string()).collect();
    front.target = target.map(str::to_string);
    let body = crate::letter::fixture::need_body_with(&front, "prose");
    let sent = rig.mail.send_step("pump", "pump", "the need", &body, &[])?;
    if let Some(task) = rig.mail.tasks.remove(&sent) {
        rig.mail.tasks.insert(NEED.to_string(), task);
    }
    Ok(())
}

#[test]
fn effective_scope_is_present_even_when_thread_is_capped() -> R {
    // An explicit target map: inserting beside the legacy default keeps it
    // configured.
    let mut cfg = PumpConfig::default();
    cfg.targets
        .insert(String::from("default"), PumpTarget::default());
    cfg.targets
        .insert(String::from("web"), PumpTarget::default());
    let mut rig = rig_with(cfg)?;
    seed_need(&mut rig, &["src/pump/"], Some("default"))?;
    ruling(
        &rig.store,
        "j1",
        "default",
        &["docs/"],
        RulingDecision::Approved,
    );
    // Another target's grant must never reach the default block.
    ruling(
        &rig.store,
        "j2",
        "web",
        &["skills/"],
        RulingDecision::Approved,
    );
    // A thread past the 40 000-character cap: the ruling is named only in the
    // dropped tail, so the brief cannot be quoting thread prose.
    rig.host.reply(
        "need show",
        0,
        &format!("{}\nruling approves docs/", "x".repeat(40_001)),
    );
    rig.planned("plan", "PLAN ONE TEXT")?;

    let spec = job(rig.start("admit1")?)?;

    let at = spec.body.find("\nthread:\n").expect("thread block");
    let (head, thread) = (&spec.body[..at], &spec.body[at..]);
    assert!(head.contains("AUTHORITATIVE SCOPE AND POLICY"), "{}", head);
    assert!(head.contains("- src/pump/"), "{head}");
    assert!(head.contains("APPROVED ADDITIONS on default"), "{head}");
    assert!(head.contains("- docs/"), "{head}");
    assert!(head.contains("CURRENT POLICY of default"), "{head}");
    assert!(head.contains("allow: ./"), "{head}");
    assert!(!head.contains("skills/"), "another target's grant: {head}");
    assert!(!thread.contains("docs/"), "capped thread: {thread}");
    Ok(())
}

#[test]
fn an_unspecified_original_scope_stays_unspecified() -> R {
    let mut rig = rig()?;
    rig.planned("plan", "PLAN ONE TEXT")?;

    let spec = job(rig.start("admit1")?)?;

    assert!(
        spec.body.contains("ORIGINAL NEED SCOPE: unspecified"),
        "{}",
        spec.body
    );
    assert!(spec.body.contains("not a deny-all"), "{}", spec.body);
    Ok(())
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
