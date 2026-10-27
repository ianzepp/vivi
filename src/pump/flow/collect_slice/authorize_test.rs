//! Tests for need-level authorization of hand blocked-scope requests: the
//! grant or the operator hold lands before any slice coordination.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use crate::error::VivariumError;
use crate::letter::fixture::{need_body_with, record_body, task_body_with};
use crate::letter::work::{NeedFront, TaskFront};
use crate::mailspace::shipped::SLICER_ROLE;
use crate::pump::config::{PumpConfig, PumpTarget};
use crate::pump::flow::advance::start::start;
use crate::pump::flow::collect_step::RunCx;
use crate::pump::flow::def::FlowKind;
use crate::pump::flow::fake_mail::{Call, FakeMail};
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{StepJobRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::need_branch;
use crate::pump::flow::need_scope::{RulingDecision, ScopeIssue};
use crate::pump::flow::reslice;
use crate::pump::flow::scope::{BlockedScope, ScopeState};
use crate::pump::flow::slice_dispatch;
use crate::pump::flow::tick::OPERATOR_HOLD;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

const NEED: &str = "n1";
const BASE: &str = "2222222222222222222222222222222222222222";
const TARGET: &str = "web";

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
    _checkout: TempDir,
    ctx: Ctx,
    _host: Arc<FakeHost>,
    store: PumpStore,
    mail: FakeMail,
    jobs: Jobs,
}

fn role(name: &str, kind: &str, pool: &str) -> RoutedRole {
    RoutedRole {
        name: name.into(),
        kind: Some(kind.into()),
        status: String::from("active"),
        pool: Some(pool.into()),
        read_only: None,
        model: None,
        models: Vec::new(),
    }
}

/// A verifiable integration checkout with a real `src/` tree.
fn a_checkout() -> Result<TempDir, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    fs::create_dir_all(dir.path().join("src"))
        .map_err(|e| VivariumError::Other(e.to_string()))?;
    fs::write(dir.path().join("src").join("lib.rs"), "fn main() {}\n")
        .map_err(|e| VivariumError::Other(e.to_string()))?;
    Ok(dir)
}

/// The target the slices run on: `src/` is allowed, `src/secret/` is denied,
/// and requested paths verify inside `checkout`.
fn a_config(checkout: &Path) -> PumpConfig {
    let targets = BTreeMap::from([(
        String::from(TARGET),
        PumpTarget {
            scope_allow: Some(vec![String::from("src/")]),
            scope_deny: vec![String::from("src/secret/")],
            integration_repo: Some(checkout.to_string_lossy().into_owned()),
            ..PumpTarget::default()
        },
    )]);
    PumpConfig {
        targets,
        ..PumpConfig::default()
    }
}

fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let checkout = a_checkout()?;
    let (ctx, host) = test_ctx(dir.path(), a_config(checkout.path()));
    let mut routing = (*ctx.routing).clone();
    routing.roles.push(role("hand", "hand", "test-planner-pool"));
    routing
        .roles
        .push(role(SLICER_ROLE, "slicer", "test-slicer-pool"));
    let ctx = ctx.with_routing(routing);
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    store.upsert_need_branch(&NeedBranchRow {
        need: NEED.into(),
        branch: need_branch::branch(NEED),
        base: Some(BASE.into()),
        status: need_branch::LIVE.into(),
        run: 1,
        target: TARGET.into(),
        priors: Vec::new(),
    })?;
    let mut mail = FakeMail::new("mind");
    MailWriter::open_record(&mut mail, NEED, &record_body(NEED, "build"))?;
    seed_need_letter(&mut mail)?;
    Ok(Rig {
        _dir: dir,
        _checkout: checkout,
        ctx,
        _host: host,
        store,
        mail,
        jobs: Jobs::default(),
    })
}

/// The need's root letter: an immutable delegation of `src/a.rs` on no named
/// target, so the branch row's target decides. Authorization reads it for
/// the need's target and standing scope.
fn seed_need_letter(mail: &mut FakeMail) -> R {
    let front = NeedFront {
        write_scope: vec![String::from("src/a.rs")],
        ..NeedFront::new(vec![String::from("done")])
    };
    let sent = mail.send_step("mind", "hand", "the need", &need_body_with(&front, ""), &[])?;
    if let Some(task) = mail.tasks.remove(&sent) {
        mail.tasks.insert(NEED.to_string(), task);
    }
    Ok(())
}

impl Rig {
    /// A running slice of the live run with `path` in its write scope.
    fn add_slice(&mut self, id: &str, path: &str) -> Result<StepRow, VivariumError> {
        let task = self.mail.send_step(
            "mind",
            "mind",
            &format!("slice {id}"),
            &task_body_with(
                &TaskFront {
                    target: Some(TARGET.into()),
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
            .insert_slice_scope(&task, 1, &[path.to_string()])?;
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

/// A started run with one dispatched slice holding `src/a.rs`.
fn one_slice(rig: &mut Rig) -> Result<(StepRow, String), VivariumError> {
    start(&rig.ctx, &rig.store, &mut rig.mail, NEED, Some("plan"))?;
    let slice = rig.add_slice("a", "src/a.rs")?;
    let job = rig.dispatch(&slice)?;
    Ok((slice, job))
}

/// A started run with two dispatched slices: `a` holds `src/a.rs`, `b`
/// holds `src/b.rs`.
fn two_slices(rig: &mut Rig) -> Result<(StepRow, StepRow, String, String), VivariumError> {
    start(&rig.ctx, &rig.store, &mut rig.mail, NEED, Some("plan"))?;
    let a = rig.add_slice("a", "src/a.rs")?;
    let b = rig.add_slice("b", "src/b.rs")?;
    let ja = rig.dispatch(&a)?;
    let jb = rig.dispatch(&b)?;
    Ok((a, b, ja, jb))
}

fn a_request(paths: &[&str], design: bool) -> BlockedScope {
    BlockedScope {
        paths: paths.iter().map(|path| (*path).to_string()).collect(),
        reason: String::from("needs it"),
        design,
    }
}

/// The step_jobs row of the finished hand job `job`.
fn hand_row(rig: &Rig, job: &str) -> Result<StepJobRow, VivariumError> {
    rig.store
        .step_job(job)?
        .ok_or_else(|| VivariumError::Message(format!("no step job {job}")))
}

/// The collector's settlement of a blocked-scope answer from hand job `job`.
fn block_scope(
    rig: &mut Rig,
    slice: &StepRow,
    job: &str,
    request: &BlockedScope,
) -> Result<(), VivariumError> {
    let row = hand_row(rig, job)?;
    with_cx(rig, |cx| cx.blocked(slice, &row, job, request))
}

/// A cx over the rig's run 1, for calling collection helpers directly.
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

fn step_of(rig: &Rig, slice: &StepRow) -> Result<StepRow, VivariumError> {
    rig.store
        .step_by_task(&slice.task)?
        .ok_or_else(|| VivariumError::Message("no step row".into()))
}

/// The body of the newest note posted to the need's audience (the run's
/// start note is posted first).
fn posted_ruling(rig: &Rig) -> Result<String, VivariumError> {
    rig.mail
        .calls
        .iter()
        .rev()
        .find_map(|call| match call {
            Call::PostToNeed { body, .. } => Some(body.clone()),
            _ => None,
        })
        .ok_or_else(|| VivariumError::Message("no ruling note was posted".into()))
}

fn exceptions(rig: &Rig) -> String {
    fs::read_to_string(rig.ctx.home.join("exceptions.txt")).unwrap_or_default()
}

fn journaled(rig: &Rig, source: &str) -> Result<crate::pump::store::ScopeJournalRow, VivariumError> {
    rig.store
        .scope_request(source)?
        .ok_or_else(|| VivariumError::Message(format!("no journal row for {source}")))
}

#[test]
fn an_allowed_request_grants_at_need_level_then_widens_and_redispatches() -> R {
    let mut rig = rig()?;
    let (slice, job) = one_slice(&mut rig)?;
    let before = rig.jobs.jobs.len();

    block_scope(&mut rig, &slice, &job, &a_request(&["src/extra.rs"], false))?;

    let again = format!("{job}-s1");
    let redispatched = rig
        .jobs
        .jobs
        .get(&again)
        .ok_or_else(|| VivariumError::Message(format!("no redispatch {again}")))?;
    assert!(
        redispatched.body.contains("- src/extra.rs"),
        "{}",
        redispatched.body
    );
    assert_eq!(rig.jobs.jobs.len(), before + 1);
    let row = journaled(&rig, &job)?;
    assert_eq!(row.decision, RulingDecision::Grant);
    assert!(row.published_at.is_some(), "permission is on the record");
    assert_eq!(row.round, Some(1));
    assert!(posted_ruling(&rig)?.contains("src/extra.rs"));
    Ok(())
}

#[test]
fn a_denied_request_holds_the_slice_for_the_operator_and_schedules_nothing() -> R {
    let mut rig = rig()?;
    let (slice, job) = one_slice(&mut rig)?;
    let before = rig.jobs.jobs.len();

    block_scope(&mut rig, &slice, &job, &a_request(&["src/secret/key.rs"], false))?;

    let state = step_of(&rig, &slice)?;
    assert_eq!(
        (state.state, state.verdict.as_deref()),
        (StepState::Waiting, Some(OPERATOR_HOLD))
    );
    assert!(
        exceptions(&rig).contains("flow-scope-operator"),
        "{}",
        exceptions(&rig)
    );
    let snapshot = rig.store.scope_coordination_snapshot(&slice.task, 1)?;
    assert_eq!(snapshot.requests, 0, "no scheduling request was counted");
    assert_eq!(rig.store.scope_state(&slice.task)?, Some(ScopeState::Clear));
    assert!(rig.mail.tasks[&slice.task].depends_on.is_empty());
    assert_eq!(rig.jobs.jobs.len(), before, "nothing dispatched");
    let row = journaled(&rig, &job)?;
    assert_eq!(row.decision, RulingDecision::OperatorRequired);
    assert_eq!(row.issue, Some(ScopeIssue::Denied));
    assert!(row.published_at.is_some(), "the ruling is published");
    Ok(())
}

#[test]
fn a_request_straddling_denied_scope_is_refused_whole() -> R {
    let mut rig = rig()?;
    let (slice, job) = one_slice(&mut rig)?;
    let before = rig.jobs.jobs.len();

    block_scope(
        &mut rig,
        &slice,
        &job,
        &a_request(&["src/extra.rs", "src/secret/key.rs"], false),
    )?;

    let state = step_of(&rig, &slice)?;
    assert_eq!(state.verdict.as_deref(), Some(OPERATOR_HOLD));
    assert_eq!(rig.jobs.jobs.len(), before, "nothing dispatched");
    let row = journaled(&rig, &job)?;
    assert_eq!(row.decision, RulingDecision::OperatorRequired);
    assert_eq!(
        row.paths,
        vec![
            String::from("src/extra.rs"),
            String::from("src/secret/key.rs")
        ]
    );
    assert_eq!(rig.store.scope_state(&slice.task)?, Some(ScopeState::Clear));
    Ok(())
}

#[test]
fn a_mixed_request_keeps_what_is_held_and_grants_only_the_additions() -> R {
    let mut rig = rig()?;
    let (slice, job) = one_slice(&mut rig)?;

    block_scope(
        &mut rig,
        &slice,
        &job,
        &a_request(&["src/a.rs", "src/new.rs"], false),
    )?;

    let posted = posted_ruling(&rig)?;
    assert!(posted.contains("src/new.rs"), "{posted}");
    assert!(!posted.contains("src/a.rs"), "{posted}");
    let row = journaled(&rig, &job)?;
    assert_eq!(row.decision, RulingDecision::Grant);
    assert_eq!(
        row.paths,
        vec![String::from("src/a.rs"), String::from("src/new.rs")]
    );
    let again = format!("{job}-s1");
    assert!(rig.jobs.jobs.contains_key(&again), "scheduling still ran");
    Ok(())
}

#[test]
fn a_corrected_hand_authorizes_under_its_original_source() -> R {
    let mut rig = rig()?;
    let (slice, job) = one_slice(&mut rig)?;
    let corrected = format!("{job}-c1");
    rig.store.insert_step_job(
        NEED,
        &StepJobRow {
            job: corrected.clone(),
            run: 1,
            node: slice.node.clone(),
            task: slice.task.clone(),
            attempt: 1,
        },
    )?;

    block_scope(&mut rig, &slice, &corrected, &a_request(&["src/extra.rs"], false))?;

    let row = journaled(&rig, &corrected)?;
    assert_eq!(row.source, job, "the correction answers for its base job");
    assert!(rig.store.scope_request(&job)?.is_some());
    let again = format!("{job}-s1");
    assert!(rig.jobs.jobs.contains_key(&again));
    Ok(())
}

#[test]
fn a_failed_publication_schedules_nothing() -> R {
    let mut rig = rig()?;
    let (slice, job) = one_slice(&mut rig)?;
    let before = rig.jobs.jobs.len();
    // The next mail write is the ruling note's publication: it fails once.
    rig.mail.fail_at = Some(rig.mail.calls.len());

    let error = block_scope(&mut rig, &slice, &job, &a_request(&["src/extra.rs"], false))
        .expect_err("the publication failed");

    assert!(error.to_string().contains("fake mail"), "{error}");
    assert_eq!(rig.jobs.jobs.len(), before, "nothing dispatched");
    let state = step_of(&rig, &slice)?;
    assert_eq!(state.state, StepState::Running, "no operator hold");
    assert!(exceptions(&rig).is_empty(), "{}", exceptions(&rig));
    let row = journaled(&rig, &job)?;
    assert!(row.published_at.is_none(), "never acknowledged");
    Ok(())
}

#[test]
fn an_allowed_request_held_by_an_open_slice_waits_on_it() -> R {
    let mut rig = rig()?;
    let (a, b, ja, _jb) = two_slices(&mut rig)?;
    let before = rig.jobs.jobs.len();

    block_scope(&mut rig, &a, &ja, &a_request(&["src/b.rs"], false))?;

    assert!(rig.mail.tasks[&a.task].depends_on.contains(&b.task));
    let state = step_of(&rig, &a)?;
    assert_eq!(
        (state.state, state.verdict.as_deref()),
        (StepState::Waiting, None)
    );
    assert_eq!(rig.store.scope_state(&a.task)?, Some(ScopeState::Wait));
    assert_eq!(rig.jobs.jobs.len(), before, "waiting queues nothing");
    let row = journaled(&rig, &ja)?;
    assert_eq!(row.decision, RulingDecision::Grant);
    assert!(row.published_at.is_some(), "permission came first");
    Ok(())
}

#[test]
fn a_design_request_is_granted_then_sent_to_rework() -> R {
    let mut rig = rig()?;
    let (slice, job) = one_slice(&mut rig)?;
    let before = rig.jobs.jobs.len();

    block_scope(&mut rig, &slice, &job, &a_request(&["src/extra.rs"], true))?;

    let row = journaled(&rig, &job)?;
    assert_eq!(row.decision, RulingDecision::Grant, "a path grant only");
    assert!(row.published_at.is_some());
    assert!(row.design);
    assert_eq!(
        rig.store.scope_state(&slice.task)?,
        Some(ScopeState::Rework)
    );
    assert!(
        exceptions(&rig).contains("flow-scope-rework"),
        "{}",
        exceptions(&rig)
    );
    let resliced: Vec<&PoolJob> = rig
        .jobs
        .jobs
        .values()
        .filter(|job| job.kind == reslice::KIND)
        .collect();
    assert_eq!(resliced.len(), 1);
    assert_eq!(resliced[0].payload["mode"], "rework");
    assert!(resliced[0].body.contains("src/extra.rs"));
    assert_eq!(rig.jobs.jobs.len(), before + 1);
    Ok(())
}
