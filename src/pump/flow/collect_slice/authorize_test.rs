//! Companion tests: authorizing a hand's blocked-scope request at need level
//! before slice coordination — allowed, denied and mixed requests, the
//! corrected hand job as the journal identity, a failed publication that
//! dispatches nothing, a holder wait as scheduling only, and a design
//! question that reworks after its grant.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::FLOW_SCOPE_OPERATOR;
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
use crate::pump::flow::need_scope::RulingDecision;
use crate::pump::flow::reslice;
use crate::pump::flow::scope::{BlockedScope, ScopeState};
use crate::pump::flow::tick::OPERATOR_HOLD;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobStore, PoolJob};
use crate::pump::store::{
    DB_FILENAME, NeedBranchRow, PumpStore, ScopeCoordination, ScopeJournalRow,
};
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

const NEED: &str = "n1";
const TARGET: &str = "default";
/// The pump's reply role: the only sender whose scope rulings count.
const REPLY_FROM: &str = "mind";
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
        &self.jobs
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

/// One configured target: `src/` allowed, `src/secret/` denied, and the real
/// integration checkout the requests are verified against.
fn a_target(checkout: &Path) -> PumpTarget {
    PumpTarget {
        scope_allow: Some(vec![String::from("src/")]),
        scope_deny: vec![String::from("src/secret/")],
        integration_repo: Some(checkout.to_string_lossy().into_owned()),
        ..PumpTarget::default()
    }
}

fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let checkout = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    fs::create_dir_all(checkout.path().join("src/secret"))
        .map_err(|e| VivariumError::Other(e.to_string()))?;
    fs::write(checkout.path().join("src/lib.rs"), "fn main() {}\n")
        .map_err(|e| VivariumError::Other(e.to_string()))?;
    let targets = BTreeMap::from([(String::from(TARGET), a_target(checkout.path()))]);
    let (ctx, host) = test_ctx(
        dir.path(),
        PumpConfig {
            targets,
            ..PumpConfig::default()
        },
    );
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
    routing.roles.push(RoutedRole {
        name: String::from(SLICER_ROLE),
        kind: Some(String::from("slicer")),
        status: String::from("active"),
        pool: Some(String::from("test-slicer-pool")),
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
        target: TARGET.into(),
        priors: Vec::new(),
    })?;
    let mut mail = FakeMail::new(REPLY_FROM);
    MailWriter::open_record(&mut mail, NEED, &record_body(NEED, "build"))?;
    seed_need_root(&mut mail);
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

/// The immutable need root of the run: the letter delegates `docs/` on the
/// default target, so `src/` blocked-scope asks are real grants over the
/// delegation. Sent as the pump and re-keyed to the need handle, the way a
/// mailspace keeps the need thread at one handle.
fn seed_need_root(mail: &mut FakeMail) {
    let front = NeedFront {
        write_scope: vec![String::from("docs/")],
        target: Some(String::from(TARGET)),
        ..NeedFront::new(vec![String::from("done")])
    };
    let sent = mail
        .send_step("mind", "hand", "the need", &need_body_with(&front, ""), &[])
        .expect("need root letter");
    if let Some(task) = mail.tasks.remove(&sent) {
        mail.tasks.insert(NEED.to_string(), task);
    }
}

/// A pending slice of run 1 with `scope`, named `id` on the default target.
fn a_slice(rig: &mut Rig, id: &str, scope: &str) -> Result<StepRow, VivariumError> {
    let task = rig.mail.send_step(
        "mind",
        "mind",
        &format!("slice {id}"),
        &task_body_with(
            &TaskFront {
                target: Some(String::from(TARGET)),
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
        state: StepState::Pending,
        verdict: None,
    };
    rig.store.insert_step(NEED, &slice)?;
    rig.store.insert_slice_scope(&task, 1, &[scope.to_string()])?;
    Ok(slice)
}

/// A step_jobs row naming `job` as the hand job of `slice`.
fn a_row(rig: &Rig, slice: &StepRow, job: &str) -> Result<StepJobRow, VivariumError> {
    let row = StepJobRow {
        job: job.to_string(),
        run: 1,
        node: slice.node.clone(),
        task: slice.task.clone(),
        attempt: 1,
    };
    rig.store.insert_step_job(NEED, &row)?;
    Ok(row)
}

/// A blocked-scope ask for `paths`.
fn a_request(paths: &[&str], reason: &str) -> BlockedScope {
    BlockedScope {
        paths: paths.iter().map(|path| (*path).to_string()).collect(),
        reason: reason.to_string(),
        design: false,
    }
}

/// The step row of `slice`.
fn step_of(rig: &Rig, slice: &StepRow) -> Result<StepRow, VivariumError> {
    rig.store
        .step_by_task(&slice.task)?
        .ok_or_else(|| VivariumError::Message("no step row".into()))
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

/// The recorded need-audience posts as (need, body).
fn posts(mail: &FakeMail) -> Vec<(String, String)> {
    mail.calls
        .iter()
        .filter_map(|call| match call {
            Call::PostToNeed { need, body } => Some((need.clone(), body.clone())),
            _ => None,
        })
        .collect()
}

/// The one recorded post's body.
fn one_post(rig: &Rig) -> Result<String, VivariumError> {
    let posts = posts(&rig.mail);
    let [(need, body)] = posts.as_slice() else {
        return Err(VivariumError::Message(format!(
            "expected one ruling post; got {}",
            posts.len()
        )));
    };
    assert_eq!(need, NEED);
    Ok(body.clone())
}

fn exceptions(rig: &Rig) -> String {
    fs::read_to_string(rig.ctx.home.join("exceptions.txt")).unwrap_or_default()
}

fn journal(rig: &Rig, source: &str) -> Result<ScopeJournalRow, VivariumError> {
    rig.store
        .scope_request(source)?
        .ok_or_else(|| VivariumError::Message(format!("no journal row for {source}")))
}

#[test]
fn an_allowed_request_grants_at_need_level_then_widens_and_redispatches() -> R {
    let mut rig = rig()?;
    start(&rig.ctx, &rig.store, &mut rig.mail, NEED, Some("plan"))?;
    let slice = a_slice(&mut rig, "a", "src/a.rs")?;
    let row = a_row(&rig, &slice, "hand-j1")?;

    with_cx(&mut rig, |cx| {
        cx.blocked(&slice, &row, &a_request(&["src/extra.rs"], "needs the file"))
    })?;

    let body = one_post(&rig)?;
    assert!(body.contains("decision = \"grant\""), "{body}");
    assert!(body.contains("src/extra.rs"), "{body}");
    let record = journal(&rig, "hand-j1")?;
    assert_eq!(record.decision, RulingDecision::Grant);
    assert!(record.published_at.is_some());
    assert_eq!(record.paths, vec!["src/extra.rs".to_string()]);

    let scope = rig.store.slice_scope(&slice.task)?;
    assert!(scope.is_some_and(|scope| scope.contains(&"src/extra.rs".to_string())));
    assert_eq!(rig.store.scope_state(&slice.task)?, Some(ScopeState::Clear));
    let s1 = format!("slice-{}-a1-s1", slice.task);
    let again = rig
        .jobs
        .jobs
        .get(&s1)
        .ok_or_else(|| VivariumError::Message("no re-dispatch job".into()))?;
    assert!(again.body.contains("- src/extra.rs"), "{}", again.body);
    Ok(())
}

#[test]
fn a_denied_request_publishes_an_operator_ruling_and_holds_without_scheduling() -> R {
    let mut rig = rig()?;
    start(&rig.ctx, &rig.store, &mut rig.mail, NEED, Some("plan"))?;
    let slice = a_slice(&mut rig, "a", "src/a.rs")?;
    let row = a_row(&rig, &slice, "hand-j1")?;
    let before = rig.jobs.jobs.len();
    let reason = "needs the secret";

    with_cx(&mut rig, |cx| {
        cx.blocked(&slice, &row, &a_request(&["src/secret/x.rs"], reason))
    })?;

    let body = one_post(&rig)?;
    assert!(body.contains("decision = \"operator-required\""), "{body}");
    assert!(body.contains("issue = \"denied\""), "{body}");
    let record = journal(&rig, "hand-j1")?;
    assert_eq!(record.decision, RulingDecision::OperatorRequired);
    assert!(record.published_at.is_some());

    let step = step_of(&rig, &slice)?;
    assert_eq!(step.state, StepState::Waiting);
    assert_eq!(step.verdict.as_deref(), Some(OPERATOR_HOLD));
    assert_eq!(rig.store.scope_state(&slice.task)?, Some(ScopeState::Clear));
    assert_eq!(
        rig.store.slice_scope(&slice.task)?,
        Some(vec!["src/a.rs".to_string()]),
        "nothing widened"
    );
    assert_eq!(rig.jobs.jobs.len(), before, "nothing queued");
    let alarm = exceptions(&rig);
    assert!(alarm.contains(FLOW_SCOPE_OPERATOR), "{alarm}");

    // A replay neither re-alarms nor schedules.
    with_cx(&mut rig, |cx| {
        cx.blocked(&slice, &row, &a_request(&["src/secret/x.rs"], reason))
    })?;
    assert_eq!(
        exceptions(&rig).matches(FLOW_SCOPE_OPERATOR).count(),
        1,
        "the alarm is raised once"
    );
    assert_eq!(rig.jobs.jobs.len(), before);
    Ok(())
}

#[test]
fn a_mixed_request_is_refused_whole_with_nothing_granted() -> R {
    let mut rig = rig()?;
    start(&rig.ctx, &rig.store, &mut rig.mail, NEED, Some("plan"))?;
    let slice = a_slice(&mut rig, "a", "src/a.rs")?;
    let row = a_row(&rig, &slice, "hand-j1")?;
    let before = rig.jobs.jobs.len();

    with_cx(&mut rig, |cx| {
        cx.blocked(
            &slice,
            &row,
            &a_request(&["src/lib.rs", "src/secret/x.rs"], "needs both"),
        )
    })?;

    let body = one_post(&rig)?;
    assert!(body.contains("decision = \"operator-required\""), "{body}");
    let record = journal(&rig, "hand-j1")?;
    assert_eq!(record.decision, RulingDecision::OperatorRequired);
    assert_eq!(
        record.paths,
        vec!["src/lib.rs".to_string(), "src/secret/x.rs".to_string()],
        "the whole request is ruled on"
    );
    assert_eq!(
        rig.store.slice_scope(&slice.task)?,
        Some(vec!["src/a.rs".to_string()]),
        "no member is granted"
    );
    let step = step_of(&rig, &slice)?;
    assert_eq!(step.state, StepState::Waiting);
    assert_eq!(step.verdict.as_deref(), Some(OPERATOR_HOLD));
    assert_eq!(rig.jobs.jobs.len(), before);
    Ok(())
}

#[test]
fn a_corrected_hand_journals_under_its_original_source_and_a_replay_adopts() -> R {
    let mut rig = rig()?;
    start(&rig.ctx, &rig.store, &mut rig.mail, NEED, Some("plan"))?;
    let slice = a_slice(&mut rig, "a", "src/a.rs")?;
    let row = a_row(&rig, &slice, "hand-j1-c1")?;

    with_cx(&mut rig, |cx| {
        cx.blocked(&slice, &row, &a_request(&["src/extra.rs"], "needs the file"))
    })?;

    assert!(rig.store.scope_request("hand-j1-c1")?.is_none());
    let record = journal(&rig, "hand-j1")?;
    assert!(record.published_at.is_some());
    let body = one_post(&rig)?;
    assert!(body.contains("hand-j1"), "{body}");
    let s1 = format!("slice-{}-a1-s1", slice.task);
    assert!(rig.jobs.jobs.contains_key(&s1), "{:?}", rig.jobs.jobs.keys());

    // The published ruling lands on the need thread, as real mail would.
    rig.mail.reply(NEED, &body)?;

    let scope_before = rig.store.slice_scope(&slice.task)?;
    let jobs_before = rig.jobs.jobs.len();
    with_cx(&mut rig, |cx| {
        cx.blocked(&slice, &row, &a_request(&["src/extra.rs"], "needs the file"))
    })?;

    assert_eq!(
        posts(&rig.mail).len(),
        1,
        "the replay adopts the published ruling"
    );
    assert_eq!(
        rig.store.slice_scope(&slice.task)?,
        scope_before,
        "nothing re-widened"
    );
    assert_eq!(
        rig.jobs.jobs.len(),
        jobs_before,
        "the re-dispatch job is adopted, not duplicated"
    );
    Ok(())
}

#[test]
fn a_failed_publication_dispatches_nothing_until_retried() -> R {
    let mut rig = rig()?;
    start(&rig.ctx, &rig.store, &mut rig.mail, NEED, Some("plan"))?;
    let slice = a_slice(&mut rig, "a", "src/a.rs")?;
    let row = a_row(&rig, &slice, "hand-j1")?;
    let before = rig.jobs.jobs.len();

    // The next mail write — the ruling post — fails.
    rig.mail.fail_at = Some(rig.mail.calls.len());
    let failed = with_cx(&mut rig, |cx| {
        cx.blocked(&slice, &row, &a_request(&["src/extra.rs"], "needs the file"))
    });
    assert!(failed.is_err(), "the failed publication returns");
    assert!(posts(&rig.mail).is_empty(), "the note did not land");
    assert_eq!(
        rig.store.slice_scope(&slice.task)?,
        Some(vec!["src/a.rs".to_string()])
    );
    assert_eq!(rig.jobs.jobs.len(), before, "nothing dispatched");
    assert_eq!(rig.store.scope_state(&slice.task)?, Some(ScopeState::Clear));

    // The prepared journal record makes the retry identical, and it succeeds.
    with_cx(&mut rig, |cx| {
        cx.blocked(&slice, &row, &a_request(&["src/extra.rs"], "needs the file"))
    })?;
    assert_eq!(posts(&rig.mail).len(), 1);
    assert!(rig.jobs.jobs.contains_key(&format!("slice-{}-a1-s1", slice.task)));
    Ok(())
}

#[test]
fn an_allowed_request_a_holder_holds_waits_as_scheduling_only() -> R {
    let mut rig = rig()?;
    start(&rig.ctx, &rig.store, &mut rig.mail, NEED, Some("plan"))?;
    let a = a_slice(&mut rig, "a", "src/a.rs")?;
    let b = a_slice(&mut rig, "b", "src/extra.rs")?;
    let row = a_row(&rig, &a, "hand-j1")?;
    let before = rig.jobs.jobs.len();

    with_cx(&mut rig, |cx| {
        cx.blocked(&a, &row, &a_request(&["src/extra.rs"], "needs the file"))
    })?;

    // The permission was established at need level anyway; the holder wait is
    // only scheduling.
    let body = one_post(&rig)?;
    assert!(body.contains("decision = \"grant\""), "{body}");
    assert!(rig.mail.tasks[&a.task].depends_on.contains(&b.task));
    assert_eq!(rig.store.scope_state(&a.task)?, Some(ScopeState::Wait));
    let step = step_of(&rig, &a)?;
    assert_eq!(step.state, StepState::Waiting);
    assert_eq!(step.verdict, None);
    assert_eq!(rig.jobs.jobs.len(), before, "a wait queues nothing");
    let record = journal(&rig, "hand-j1")?;
    assert_eq!(record.coordination, Some(ScopeCoordination::WaitOn));

    // Once the holder closes, the wait releases with the held path granted.
    rig.mail.close_task(&b.task, "merged", None, None)?;
    with_cx(&mut rig, |cx| cx.release_one(&a))?;
    let s1 = format!("slice-{}-a1-s1", a.task);
    let again = rig
        .jobs
        .jobs
        .get(&s1)
        .ok_or_else(|| VivariumError::Message("no release job".into()))?;
    assert!(again.body.contains("- src/extra.rs"), "{}", again.body);
    assert_eq!(rig.store.scope_state(&a.task)?, Some(ScopeState::Clear));
    Ok(())
}

#[test]
fn a_design_question_reworks_after_its_grant_is_published() -> R {
    let mut rig = rig()?;
    start(&rig.ctx, &rig.store, &mut rig.mail, NEED, Some("plan"))?;
    let slice = a_slice(&mut rig, "a", "src/a.rs")?;
    let row = a_row(&rig, &slice, "hand-j1")?;
    let before = rig.jobs.jobs.len();

    with_cx(&mut rig, |cx| {
        cx.blocked(
            &slice,
            &row,
            &BlockedScope {
                paths: vec!["src/extra.rs".to_string()],
                reason: "the slices must share a module edge".into(),
                design: true,
            },
        )
    })?;

    // The path grant is published anyway: permission and the design question
    // are different decisions.
    let body = one_post(&rig)?;
    assert!(body.contains("decision = \"grant\""), "{body}");
    let record = journal(&rig, "hand-j1")?;
    assert!(record.design);
    assert_eq!(record.coordination, Some(ScopeCoordination::Rework));
    assert_eq!(rig.store.scope_state(&slice.task)?, Some(ScopeState::Rework));
    let resliced = rig
        .jobs
        .jobs
        .values()
        .filter(|job| job.kind == reslice::KIND)
        .collect::<Vec<_>>();
    assert_eq!(resliced.len(), 1);
    assert_eq!(resliced[0].payload["mode"], "rework");
    assert!(resliced[0].body.contains("src/extra.rs"), "{}", resliced[0].body);
    assert_eq!(rig.jobs.jobs.len(), before + 1, "the reslice is the only job");
    Ok(())
}
