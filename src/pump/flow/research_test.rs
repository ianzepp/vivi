//! The researcher step handler: the `for:` line and the brief it builds.

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

/// A rig whose need task carries `body`; `with_role` adds the researcher.
fn rig(body: &str, with_role: bool) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
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
fn for_want_reads_the_first_for_line() {
    assert_eq!(for_want("x\nfor: w1\nfor: w2"), Some("w1".to_owned()));
    assert_eq!(for_want("for: <want-1234>"), Some("want-1234".to_owned()));
    assert_eq!(
        for_want("  for: `want-1234` "),
        Some("want-1234".to_owned())
    );
    assert_eq!(for_want("no such line\nfor:"), None);
    assert_eq!(for_want("plain body"), None);
}

#[test]
fn for_want_reads_a_front_matter_explores() {
    let mut front = crate::letter::work::NeedFront::new(vec![String::from("answered")]);
    front.explores = Some(String::from("want-9"));
    let body = crate::letter::fixture::need_body_with(&front, "Look into it.");
    assert_eq!(for_want(&body), Some("want-9".to_owned()));
    let plain = crate::letter::fixture::need_body(&["answered"], "Look into it.");
    assert_eq!(for_want(&plain), None);
}

#[test]
fn a_research_step_with_a_want_gives_a_read_only_job_with_both_threads() -> R {
    let rig = rig("Look into it.\nfor: w1", true)?;
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
    assert!(spec.body.contains("for: w1"), "{}", spec.body);
    assert!(spec.body.contains("Look into it."), "{}", spec.body);
    assert!(spec.body.contains("NEED-THREAD-TEXT"), "{}", spec.body);
    assert!(spec.body.contains("WANT-THREAD-TEXT"), "{}", spec.body);
    assert!(spec.body.contains(&rig.need), "{}", spec.body);
    Ok(())
}

#[test]
fn a_need_without_a_for_line_reads_no_want() -> R {
    let rig = rig("Look into it.", true)?;
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
    let rig = rig("for: w1", true)?;
    rig.host.reply("need show", 1, "boom");
    rig.host.reply("want show", 0, "WANT-THREAD-TEXT");
    let spec = start_job(&rig)?;
    assert!(spec.body.contains("could not be read"), "{}", spec.body);
    assert!(!spec.body.contains("boom"), "{}", spec.body);
    assert!(spec.body.contains("WANT-THREAD-TEXT"), "{}", spec.body);
    Ok(())
}

#[test]
fn no_researcher_role_is_an_error() -> R {
    let rig = rig("for: w1", false)?;
    let error = start_job(&rig).err().map(|error| error.to_string());
    let text = error.ok_or_else(|| VivariumError::Message(String::from("expected an error")))?;
    assert!(text.contains("researcher"), "{text}");
    Ok(())
}

#[test]
fn a_long_thread_keeps_its_newest_text() -> R {
    let rig = rig("plain", true)?;
    let long = format!("OLDEST {} NEWEST", "x".repeat(30 * 1024));
    rig.host.reply("need show", 0, &long);
    let spec = start_job(&rig)?;
    assert!(spec.body.contains("NEWEST"), "len {}", spec.body.len());
    assert!(!spec.body.contains("OLDEST"));
    assert!(spec.body.contains("older text cut"));
    Ok(())
}
