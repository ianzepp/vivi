//! The researcher step handler: the `explores` fact and the brief it builds.

use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::{for_want, register};
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::flow::def::FlowKind;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::outcome::{StartAction, StartCx};
use crate::pump::host::{FakeHost, test_ctx};
use crate::pump::jobs::{JobSpec, JobStore};
use crate::pump::store::{DB_FILENAME, PumpStore};
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

#[derive(Default)]
struct Jobs {
    jobs: std::collections::BTreeMap<String, crate::pump::jobs::PoolJob>,
}

impl JobStore for Jobs {
    fn jobs(&self) -> &std::collections::BTreeMap<String, crate::pump::jobs::PoolJob> {
        &self.jobs
    }

    fn jobs_mut(&mut self) -> &mut std::collections::BTreeMap<String, crate::pump::jobs::PoolJob> {
        &mut self.jobs
    }
}

struct Rig {
    _dir: TempDir,
    ctx: crate::pump::host::Ctx,
    host: Arc<FakeHost>,
    store: PumpStore,
    mail: FakeMail,
    need: String,
}

fn researcher() -> RoutedRole {
    RoutedRole {
        name: String::from("researcher"),
        kind: Some(String::from("researcher")),
        status: String::from("active"),
        pool: Some(String::from("test-planner-pool")),
        read_only: None,
        model: None,
        models: Vec::new(),
    }
}

/// A need body that explores `want` (when given) over `prose`.
fn need_body(want: Option<&str>, prose: &str) -> String {
    let mut front = crate::letter::work::NeedFront::new(vec![String::from("answered")]);
    front.explores = want.map(str::to_owned);
    crate::letter::fixture::need_body_with(&front, prose)
}

/// A rig whose need task carries `body`; `with_role` adds the researcher.
fn rig(body: &str, with_role: bool) -> Result<Rig, VivariumError> {
    rig_with(PumpConfig::default(), body, with_role)
}

/// A rig over `cfg`, so tests can shape the dispatch budgets.
fn rig_with(cfg: PumpConfig, body: &str, with_role: bool) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, host) = test_ctx(dir.path(), cfg);
    let ctx = if with_role {
        let mut routing = (*ctx.routing).clone();
        routing.roles.push(researcher());
        ctx.with_routing(routing)
    } else {
        ctx
    };
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("mind");
    let need = mail.send_step("mind", "mind", "the need", body, &[])?;
    Ok(Rig {
        _dir: dir,
        ctx,
        host,
        store,
        mail,
        need,
    })
}

fn start_job(rig: &Rig) -> Result<JobSpec, VivariumError> {
    let run = RunRow {
        need: rig.need.clone(),
        run: 1,
        flow: String::from("research"),
        flow_version: 2,
        kind: String::from("research"),
        entry: String::from("research"),
        state: String::from("live"),
    };
    let step = StepRow {
        run: 1,
        node: String::from("research"),
        task: String::from("t-step"),
        state: StepState::Pending,
        verdict: None,
    };
    let def = load(FlowKind::Research)?;
    let mut registry: Registry<Jobs, FakeMail> = Registry::new();
    register(&mut registry);
    let node = def
        .nodes
        .iter()
        .find(|node| node.id == "research")
        .ok_or_else(|| VivariumError::Message(String::from("no research node")))?;
    let handler = registry
        .handler_for(FlowKind::Research, node)
        .ok_or_else(|| VivariumError::Message(String::from("no handler")))?;
    let cx = StartCx {
        ctx: &rig.ctx,
        store: &rig.store,
        mail: &rig.mail,
        run: &run,
        def: &def,
    };
    match handler.start(&cx, &step)? {
        StartAction::Job(spec) => Ok(*spec),
        other => Err(VivariumError::Message(format!("not a job: {other:?}"))),
    }
}

#[test]
fn a_body_with_no_front_matter_is_an_error() {
    assert!(for_want("x\nfor: w1\nfor: w2").is_err());
    assert!(for_want("for: <want-1234>").is_err());
    assert!(for_want("plain body").is_err());
}

#[test]
fn for_want_reads_a_front_matter_explores() -> R {
    let mut front = crate::letter::work::NeedFront::new(vec![String::from("answered")]);
    front.explores = Some(String::from("want-9"));
    let body = crate::letter::fixture::need_body_with(&front, "Look into it.");
    assert_eq!(for_want(&body)?, Some("want-9".to_owned()));
    let plain = crate::letter::fixture::need_body(&["answered"], "Look into it.");
    assert_eq!(for_want(&plain)?, None);
    Ok(())
}

#[test]
fn a_research_step_with_a_want_gives_a_read_only_job_with_both_threads() -> R {
    let rig = rig(&need_body(Some("w1"), "Look into it."), true)?;
    rig.host.reply("need show", 0, "NEED-THREAD-TEXT");
    rig.host.reply("want show", 0, "WANT-THREAD-TEXT");
    let spec = start_job(&rig)?;
    assert!(spec.read_only);
    assert_eq!(spec.from_ref.as_deref(), Some(rig.ctx.cfg.main.as_str()));
    assert_eq!(
        spec.extra_targets,
        crate::pump::flow::step_inputs::targets(&rig.ctx)
    );
    assert_eq!(spec.role, "researcher");
    assert_eq!(spec.kind, "researcher");
    assert_eq!(spec.task.as_deref(), Some("t-step"));
    assert!(spec.body.contains("explores: w1"), "{}", spec.body);
    assert!(spec.body.contains("Look into it."), "{}", spec.body);
    assert!(spec.body.contains("NEED-THREAD-TEXT"), "{}", spec.body);
    assert!(spec.body.contains("WANT-THREAD-TEXT"), "{}", spec.body);
    assert!(spec.body.contains(&rig.need), "{}", spec.body);
    Ok(())
}

#[test]
fn a_need_without_explores_reads_no_want() -> R {
    let rig = rig(&need_body(None, "Look into it."), true)?;
    rig.host.reply("need show", 0, "NEED-THREAD-TEXT");
    let spec = start_job(&rig)?;
    assert!(!spec.body.contains("WANT THREAD"), "{}", spec.body);
    assert!(
        rig.host
            .commands()
            .iter()
            .all(|command| !command.contains("want show")),
        "{:?}",
        rig.host.commands()
    );
    Ok(())
}

#[test]
fn a_failing_read_gives_the_notice_line_not_an_error() -> R {
    let rig = rig(&need_body(Some("w1"), "Look into it."), true)?;
    rig.host.reply("need show", 1, "boom");
    rig.host.reply("want show", 0, "WANT-THREAD-TEXT");
    let spec = start_job(&rig)?;
    assert!(spec.body.contains("could not be read"), "{}", spec.body);
    assert!(!spec.body.contains("boom"), "{}", spec.body);
    assert!(spec.body.contains("WANT-THREAD-TEXT"), "{}", spec.body);
    Ok(())
}

#[test]
fn a_need_that_does_not_parse_fails_the_research_step() -> R {
    let rig = rig("for: w1\nprose only", true)?;
    let error = start_job(&rig).err().map(|error| error.to_string());
    let text = error.ok_or_else(|| VivariumError::Message(String::from("expected an error")))?;
    assert!(text.contains("need"), "{text}");
    Ok(())
}

#[test]
fn no_researcher_role_is_an_error() -> R {
    let rig = rig(&need_body(Some("w1"), "Look into it."), false)?;
    let error = start_job(&rig).err().map(|error| error.to_string());
    let text = error.ok_or_else(|| VivariumError::Message(String::from("expected an error")))?;
    assert!(text.contains("researcher"), "{text}");
    Ok(())
}

#[test]
fn a_configured_researcher_budget_reaches_the_research_job() -> R {
    let mut cfg = PumpConfig::default();
    cfg.dispatch.step_budgets.researcher = Some(333);
    // Unrelated thinking steps stay distinguishable: resolving the budget
    // from any other kind must fail.
    cfg.dispatch.step_budgets.planner = Some(11);
    cfg.dispatch.step_budgets.head_cto = Some(22);
    cfg.dispatch.step_budgets.auditor = Some(44);
    cfg.dispatch.step_budgets.judge = Some(55);
    let rig = rig_with(cfg, &need_body(None, "Look into it."), true)?;
    rig.host.reply("need show", 0, "NEED-THREAD-TEXT");
    let spec = start_job(&rig)?;
    assert_eq!(spec.budget, 333);
    Ok(())
}

#[test]
fn an_absent_researcher_override_uses_the_dispatch_fallback_despite_unrelated_overrides() -> R {
    let mut cfg = PumpConfig::default();
    cfg.dispatch.budget = 4242;
    cfg.dispatch.step_budgets.planner = Some(11);
    cfg.dispatch.step_budgets.head_cto = Some(22);
    cfg.dispatch.step_budgets.auditor = Some(44);
    cfg.dispatch.step_budgets.judge = Some(55);
    let rig = rig_with(cfg, &need_body(None, "Look into it."), true)?;
    rig.host.reply("need show", 0, "NEED-THREAD-TEXT");
    let spec = start_job(&rig)?;
    assert_eq!(spec.budget, 4242);
    Ok(())
}

#[test]
fn a_long_thread_keeps_its_newest_text() -> R {
    let rig = rig(&need_body(None, "plain"), true)?;
    let long = format!("OLDEST {} NEWEST", "x".repeat(30 * 1024));
    rig.host.reply("need show", 0, &long);
    let spec = start_job(&rig)?;
    assert!(spec.body.contains("NEWEST"), "len {}", spec.body.len());
    assert!(!spec.body.contains("OLDEST"));
    assert!(spec.body.contains("older text cut"));
    Ok(())
}
