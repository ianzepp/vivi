//! A reslice request on a real mailspace is idempotent: the stored task body
//! round-trips through the mailspace, and a repeated request sees it. Both
//! base sources are exercised, including a need with no branch row at all.

use std::collections::BTreeMap;

use super::{KIND, Mode, RESLICE_NODE_PREFIX, request};
use crate::mailspace::shipped::SLICER_ROLE;
use crate::pump::fixture::project;
use crate::pump::flow::advance::start::start;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::mail::{MailWriter, MailspacePort};
use crate::pump::flow::need_branch::{self, LIVE};
use crate::pump::host::Ctx;
use crate::pump::jobs::{JobStore, PoolJob};
use crate::pump::store::flow_links::SliceLink;
use crate::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};
use crate::routing::RoutedRole;
use vivi_error::VivariumError;
use vivi_mail::letter::work::TaskFront;

type R = Result<(), VivariumError>;

/// The pump identity of the default config's `reply_from`.
const PUMP: &str = "mind";

/// The target's main ref: a reslice job starts here only as a fallback.
const MAIN: &str = "1111111111111111111111111111111111111111";
/// The published tip of the need branch and its recorded base.
const NEED_TIP: &str = "2222222222222222222222222222222222222222";
const BASE: &str = "3333333333333333333333333333333333333333";

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

/// A project with a started run and one registered slice task.
struct Fixture {
    _p: crate::pump::fixture::Project,
    need: String,
    ctx: Ctx,
    store: PumpStore,
    port: MailspacePort,
    run: RunRow,
    slice: StepRow,
}

fn fixture() -> Result<Fixture, VivariumError> {
    let p = project();
    let need = p.need("reslice me");
    let ctx = p.pump_ctx();
    let mut routing = (*ctx.routing).clone();
    routing.roles.retain(|role| role.name != SLICER_ROLE);
    routing.roles.push(RoutedRole {
        name: SLICER_ROLE.into(),
        kind: Some(String::from("slicer")),
        status: String::from("active"),
        pool: Some(String::from("test-slicer-pool")),
        read_only: Some(true),
        model: None,
        models: Vec::new(),
    });
    let ctx = ctx.with_routing(routing);
    let store = PumpStore::open(&ctx.home.join(DB_FILENAME))?;
    let mut port = MailspacePort {
        mailspace: crate::pump::record::PumpMailspace::new(p.mailspace.clone()),
        pump_identity: PUMP.into(),
    };
    MailWriter::open_record(
        &mut port,
        &need,
        &vivi_mail::letter::fixture::record_body(&need, "build"),
    )?;
    let run = start(&ctx, &store, &mut port, &need, Some("plan"))?;
    // The main ref and its ancestry are explicit: a stored SHA is not a tip.
    p.host.reply(
        "ls-remote origin refs/heads/main",
        0,
        &format!("{MAIN}\trefs/heads/main\n"),
    );
    p.host.reply("merge-base --is-ancestor", 1, "");
    let mut front = TaskFront::step(&need, run.run, "slice:a");
    front.target = Some(String::from("default"));
    let body = vivi_mail::letter::fixture::task_body_with(&front, "slice body\nsecond line");
    let slice_task = port.send_step(PUMP, PUMP, "slice a", &body, &[])?;
    let slice = StepRow {
        run: run.run,
        node: "slice:a".into(),
        task: slice_task.clone(),
        state: StepState::Running,
        verdict: None,
    };
    store.insert_step(&need, &slice)?;
    store.link_slice(&SliceLink {
        task: slice_task,
        need: need.clone(),
        run: run.run,
        fanout: Some("fan1".into()),
        deps: Vec::new(),
    })?;
    Ok(Fixture {
        _p: p,
        need,
        ctx,
        store,
        port,
        run,
        slice,
    })
}

/// The brief's metadata value for `key`.
fn metadata<'a>(brief: &'a str, key: &str) -> Option<&'a str> {
    brief
        .lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix(": "))
}

fn assert_one_request(f: &mut Fixture, source: &str, sha: &str) -> R {
    let mut jobs = Jobs::default();
    let input = "split it in two\nkeep the tests with the code\n\nthanks";
    let first = request(
        &f.ctx,
        &mut jobs,
        &f.store,
        &mut f.port,
        &f.run,
        &f.slice,
        Mode::Split,
        input,
    )?;
    let again = request(
        &f.ctx,
        &mut jobs,
        &f.store,
        &mut f.port,
        &f.run,
        &f.slice,
        Mode::Split,
        input,
    )?;

    assert_eq!(
        first, again,
        "the second request must return the first task"
    );
    let reslices: Vec<StepRow> = f
        .store
        .steps_of(&f.need, f.run.run)?
        .into_iter()
        .filter(|row| row.node.starts_with(RESLICE_NODE_PREFIX))
        .collect();
    assert_eq!(reslices.len(), 1, "{reslices:?}");
    let kinds: Vec<&PoolJob> = jobs.jobs.values().filter(|j| j.kind == KIND).collect();
    assert_eq!(kinds.len(), 1, "{kinds:?}");
    // The one job is pinned to the resolved base, and its brief says which.
    let job = kinds[0];
    assert!(job.read_only);
    assert_eq!(job.target.as_deref(), Some("default"));
    assert_eq!(job.from_ref.as_deref(), Some(sha));
    assert_eq!(metadata(&job.body, "base source"), Some(source));
    assert_eq!(metadata(&job.body, "base sha"), Some(sha));
    assert_eq!(metadata(&job.body, "base target"), Some("default"));
    Ok(())
}

#[test]
fn a_repeated_request_with_multi_line_input_sends_nothing() -> R {
    // Published need work: the need branch tip is the base.
    let mut f = fixture()?;
    f.store.upsert_need_branch(&NeedBranchRow::for_run(
        &f.run,
        need_branch::branch(&f.need),
        Some(BASE.into()),
        LIVE,
        "default",
        Vec::new(),
    ))?;
    f._p.host.reply(
        &format!(
            "ls-remote origin refs/heads/{}",
            need_branch::branch(&f.need)
        ),
        0,
        &format!("{NEED_TIP}\trefs/heads/{}\n", need_branch::branch(&f.need)),
    );
    assert_one_request(&mut f, "need-branch", NEED_TIP)?;

    // A need with no branch row at all starts from the target's main.
    let mut f = fixture()?;
    assert_one_request(&mut f, "target-main", MAIN)?;
    Ok(())
}
