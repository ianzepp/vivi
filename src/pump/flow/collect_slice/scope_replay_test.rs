//! Tests for adopting a pre-journal scope wait: the recorded hand job
//! becomes the source, the wait resumes once, and nothing is manufactured.

use std::collections::BTreeMap;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use crate::error::VivariumError;
use crate::letter::fixture::record_body;
use crate::letter::fixture::task_body_with;
use crate::letter::work::TaskFront;
use crate::pump::config::PumpConfig;
use crate::pump::flow::advance::start::start;
use crate::pump::flow::collect_step::RunCx;
use crate::pump::flow::def::FlowKind;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::need_branch;
use crate::pump::flow::scope::{HELD_PATH_PREFIX, ScopeState};
use crate::pump::flow::slice_dispatch;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

const NEED: &str = "n1";
const BASE: &str = "2222222222222222222222222222222222222222";

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
    _host: Arc<FakeHost>,
    store: PumpStore,
    mail: FakeMail,
    jobs: Jobs,
}

fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let mut routing = (*ctx.routing).clone();
    routing.roles.push(RoutedRole {
        name: String::from("hand"),
        kind: Some(String::from("hand")),
        status: String::from("active"),
        pool: Some(String::from("test-planner-pool")),
        read_only: None,
        model: None,
        models: Vec::new(),
    });
    let ctx = ctx.with_routing(routing);
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    store.upsert_need_branch(&NeedBranchRow {
        need: NEED.into(),
        branch: need_branch::branch(NEED),
        base: Some(BASE.into()),
        status: need_branch::LIVE.into(),
        run: 1,
        target: "default".into(),
        priors: Vec::new(),
    })?;
    let mut mail = FakeMail::new("mind");
    MailWriter::open_record(&mut mail, NEED, &record_body(NEED, "build"))?;
    Ok(Rig {
        _dir: dir,
        ctx,
        _host: host,
        store,
        mail,
        jobs: Jobs::default(),
    })
}

impl Rig {
    fn send_slice(&mut self, id: &str) -> Result<StepRow, VivariumError> {
        let task = self.mail.send_step(
            "mind",
            "mind",
            &format!("slice {id}"),
            &task_body_with(
                &TaskFront {
                    target: Some("default".into()),
                    ..TaskFront::step(NEED, 1, &format!("slice:{id}"))
                },
                &format!("title {id}\n\nbrief {id}"),
            ),
            &[],
        )?;
        let slice = StepRow {
            run: 1,
            node: format!("slice:{id}"),
            task: task.clone(),
            state: StepState::Running,
            verdict: None,
        };
        self.store.insert_step(NEED, &slice)?;
        self.store
            .insert_slice_scope(&task, 1, &["src/a.rs".to_string()])?;
        Ok(slice)
    }

    fn dispatch(&mut self, slice: &StepRow) -> Result<String, VivariumError> {
        slice_dispatch::dispatch(
            &self.ctx,
            &mut self.jobs,
            &self.store,
            &self.mail,
            NEED,
            slice,
            1,
        )
    }
}

/// A started run whose slice was dispatched by a binary older than the scope
/// journal: the hand job is recorded, the wait lives only in `slice_scope`.
fn legacy_wait(rig: &mut Rig) -> Result<(StepRow, String), VivariumError> {
    start(&rig.ctx, &rig.store, &mut rig.mail, NEED, Some("plan"))?;
    let slice = rig.send_slice("a")?;
    let first = rig.dispatch(&slice)?;
    rig.store.set_scope_state(&slice.task, ScopeState::Wait)?;
    rig.store
        .set_scope_holders(&slice.task, &[format!("{HELD_PATH_PREFIX}src/extra.rs")])?;
    Ok((slice, first))
}

/// A cx over the rig's run 1, for calling release helpers directly.
fn with_cx<T>(
    rig: &mut Rig,
    body: impl FnOnce(&mut RunCx<'_, Jobs, FakeMail>) -> Result<T, VivariumError>,
) -> Result<T, VivariumError> {
    let run = rig.store.runs_of(NEED)?.remove(0);
    let registry = Registry::<Jobs, FakeMail>::new();
    let decide = |_: &_, _: &_| Err(VivariumError::Message("no decision".into()));
    let mut cx = RunCx {
        ctx: &rig.ctx,
        st: &mut rig.jobs,
        store: &rig.store,
        mail: &mut rig.mail,
        registry: &registry,
        decide: &decide,
        def: load(FlowKind::Build)?,
        run,
    };
    body(&mut cx)
}

#[test]
fn legacy_scope_wait_can_resume() -> R {
    let mut rig = rig()?;
    let (slice, first) = legacy_wait(&mut rig)?;
    let before = rig.jobs.jobs.len();

    with_cx(&mut rig, |cx| cx.release_one(&slice))?;

    let again = format!("slice-{}-a1-s1", slice.task);
    let job = rig
        .jobs
        .jobs
        .get(&again)
        .ok_or_else(|| VivariumError::Message(format!("no resumed job {again}")))?;
    assert_eq!(job.task.as_deref(), Some(slice.task.as_str()));
    assert_eq!(job.branch, Some(format!("vivi/unit/{}-a1-s1", slice.task)));
    assert!(
        job.body.contains("- src/a.rs\n- src/extra.rs"),
        "the formerly held path is granted: {}",
        job.body
    );
    assert_eq!(rig.jobs.jobs.len(), before + 1);
    assert_eq!(rig.store.step_job(&again)?.unwrap().attempt, 1);
    assert_eq!(rig.store.scope_state(&slice.task)?, Some(ScopeState::Clear));
    assert!(rig.store.scope_holders(&slice.task)?.is_empty());
    assert!(
        rig.mail.tasks[&slice.task].replies.is_empty(),
        "adoption manufactures no notes"
    );

    let row = rig
        .store
        .scope_request(&first)?
        .ok_or_else(|| VivariumError::Message("no adopted journal row".into()))?;
    assert_eq!(row.source, first);
    assert_eq!(row.round, Some(1));
    assert_eq!(row.attempt, 1);
    assert_eq!(row.paths, ["src/extra.rs"]);
    assert_eq!(row.published_at, None, "adoption publishes no ruling");
    let snapshot = rig.store.scope_coordination_snapshot(&slice.task, 1)?;
    assert_eq!(snapshot.requests, 1, "the historical ask is counted once");

    // A replay of the same wait — a crash between the continuation's enqueue
    // and forgetting the wait — resumes the same continuation, not a new one.
    rig.store.set_scope_state(&slice.task, ScopeState::Wait)?;
    rig.store
        .set_scope_holders(&slice.task, &[format!("{HELD_PATH_PREFIX}src/extra.rs")])?;
    with_cx(&mut rig, |cx| cx.release_one(&slice))?;
    assert_eq!(
        rig.jobs.jobs.len(),
        before + 1,
        "the replay re-queues nothing new"
    );
    assert_eq!(rig.store.scope_state(&slice.task)?, Some(ScopeState::Clear));
    assert!(rig.store.scope_holders(&slice.task)?.is_empty());
    let snapshot = rig.store.scope_coordination_snapshot(&slice.task, 1)?;
    assert_eq!(snapshot.requests, 1, "the replay counts no second ask");
    Ok(())
}

#[test]
fn a_legacy_wait_without_a_recorded_hand_job_is_refused_not_invented() -> R {
    let mut rig = rig()?;
    start(&rig.ctx, &rig.store, &mut rig.mail, NEED, Some("plan"))?;
    let slice = rig.send_slice("a")?;
    rig.store.set_scope_state(&slice.task, ScopeState::Wait)?;

    let error = with_cx(&mut rig, |cx| cx.release_one(&slice)).unwrap_err();

    let text = error.to_string();
    assert!(
        text.contains("no recorded hand job"),
        "unexpected error: {text}"
    );
    assert_eq!(rig.jobs.jobs.len(), 0, "no request is invented");
    assert_eq!(rig.store.scope_state(&slice.task)?, Some(ScopeState::Wait));
    Ok(())
}
