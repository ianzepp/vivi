//! Tests for the durable allocation and replay of transient replacements.

#![allow(dead_code)]

use super::{allocate, Decision, Restore};
use crate::pump::fixture::{Project, T0, project};
use crate::pump::flow::budget_retry;
use crate::pump::flow::index::{self, StepJobRow, StepState};
use crate::pump::flow::recovery::{PROVIDER, RecoveryMeta, stamp_metadata};
use crate::pump::host::Ctx;
use crate::pump::jobs::{JobOutcome, JobSpec, JobStatus, JobStore, PoolJob};
use crate::pump::state::State;
use crate::pump::store::recovery::RecoveryIntentRow;
use crate::pump::store::{DB_FILENAME, PumpStore};
use crate::pump::transient;

/// The need the rig's run runs on.
const NEED: &str = "n1";

/// A collector rig: a live run with one open slice step `t1`, an empty job
/// state, and the pump store beside it.
struct Rig {
    /// Keeps the project directory alive for the whole test.
    project: Project,
    ctx: Ctx,
    store: PumpStore,
    st: State,
}

fn rig() -> Rig {
    let project = project();
    let ctx = project.pump_ctx();
    let store = PumpStore::open(&ctx.home.join(DB_FILENAME)).expect("open");
    let run = index::RunRow {
        need: NEED.into(),
        run: 3,
        flow: "research".into(),
        flow_version: index::FLOW_VERSION_NUMBER,
        kind: "research".into(),
        entry: "research".into(),
        state: index::run_state::LIVE.into(),
    };
    store.insert_run(&run).expect("insert run");
    store
        .insert_step(NEED, &run.step("slice", "t1", StepState::Running))
        .expect("insert step");
    Rig {
        project,
        ctx,
        store,
        st: State::default(),
    }
}

/// Offer `job` to the allowance; see [`super::allocate`].
fn offer(rig: &mut Rig, job: &PoolJob, binding: Option<&StepJobRow>) -> Decision {
    allocate(&rig.ctx, &mut rig.st, &rig.store, job, binding).expect("allocate")
}

/// Offer `job` and expect a fresh freeze; answers the recorded replacement
/// id and its persisted deadline.
fn queued(rig: &mut Rig, job: &PoolJob, binding: Option<&StepJobRow>) -> (String, u64) {
    match offer(rig, job, binding) {
        Decision::Queued {
            replacement,
            deadline,
        } => (replacement, deadline),
        other => panic!("expected a queued replacement, got {other:?}"),
    }
}

/// Replay `intent` through [`super::restore`].
fn replay(rig: &mut Rig, intent: &RecoveryIntentRow) -> Restore {
    super::restore(&rig.ctx, &mut rig.st, &rig.store, intent).expect("restore")
}

/// The identity payload every flow job carries: the step row's stamp.
fn identity() -> serde_json::Value {
    serde_json::json!({"need": NEED, "run": 3, "node": "slice", "task": "t1"})
}

/// A pool receipt as the pool writes it: summary lines, then sections.
fn receipt(exit: i32, message: &str, stderr: &str) -> String {
    format!(
        "pool result j: finished\nagent exit {exit} in 3s, job 9s, started from 1234567890\n\n\
         --- receipt\n{message}\n\n--- stderr tail\n{stderr}\n\n--- runner log tail\nlog"
    )
}

/// A finished job of `id` whose payload is `payload` and whose machine
/// evidence is a transient provider failure (a 429 in the stderr tail).
fn failed(id: &str, payload: serde_json::Value) -> PoolJob {
    let mut spec = JobSpec::new(id, "slice", "hand", "subject", "body");
    spec.payload = payload;
    let mut job = PoolJob::from(spec);
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: "finished".into(),
        detail: String::new(),
        receipt: receipt(1, "working on it", "429: rate limit reached"),
        at: 7,
    });
    job
}

/// A chain successor of `input` at ordinal `retry`: a failed job whose
/// payload carries the recovery metadata every payload-copying policy
/// inherits (autonomous rerun, correction, budget retry).
fn chained(id: &str, input: &str, retry: u32) -> PoolJob {
    let mut job = failed(id, identity());
    stamp_metadata(
        &mut job.payload,
        &RecoveryMeta {
            input: input.into(),
            retry,
            class: PROVIDER.into(),
        },
    );
    job
}

/// A failed job whose request carries every execution input, the step task,
/// and a spent budget-retry marker beside unrelated payload keys.
fn full_job(id: &str) -> PoolJob {
    let mut spec = JobSpec::new(id, "slice", "hand", "slice the need", "full brief");
    spec.pool_candidates = vec!["fast".into(), "slow".into()];
    spec.local_only = true;
    spec.pool_exclude = vec!["flaky".into()];
    spec.target = Some("vivi".into());
    spec.extra_targets = vec!["docs".into()];
    spec.from_ref = Some("main".into());
    spec.with = vec!["notes".into()];
    spec.model = Some("m".into());
    spec.builtin_role = Some("hand".into());
    spec.branch = Some("work".into());
    spec.allow_default_branch = true;
    spec.delete_ref = Some("refs/heads/old".into());
    spec.delete_expect = Some("sha2".into());
    spec.budget = 900;
    spec.read_only = true;
    spec.units = vec!["u1".into()];
    spec.source_jobs = vec!["j0".into()];
    spec.task = Some("t1".into());
    let mut payload = identity();
    payload["slice"] = serde_json::json!(1);
    payload["budget_retry"] = serde_json::json!(true);
    spec.payload = payload;
    let mut job = PoolJob::from(spec);
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: "finished".into(),
        detail: String::new(),
        receipt: receipt(1, "working on it", "429: rate limit reached"),
        at: 7,
    });
    job
}

/// The step-job binding of an indexed job at attempt 2 of step `t1`.
fn bound(job_id: &str) -> StepJobRow {
    StepJobRow {
        job: job_id.into(),
        run: 3,
        node: "slice".into(),
        task: "t1".into(),
        attempt: 2,
    }
}

/// The intent frozen for `replacement`, read back from the store.
fn stored(rig: &Rig, replacement: &str) -> RecoveryIntentRow {
    rig.store
        .recovery_intent(replacement)
        .expect("read intent")
        .expect("intent row")
}

#[test]
fn transient_retry_preserves_execution_inputs() {
    let mut rig = rig();
    let job = full_job("j");
    let (replacement, deadline) = queued(&mut rig, &job, Some(&bound("j")));
    assert_eq!(replacement, "j-tr1");
    assert_eq!(deadline, T0 + 60 + transient::jitter("j-tr1"));
    // The frozen request is exactly the full-input rerun copy of the failed
    // job — renamed and re-stamped, nothing else touched.
    let mut expected = budget_retry::rerun_spec(&job, &replacement);
    stamp_metadata(
        &mut expected.payload,
        &RecoveryMeta {
            input: "j".into(),
            retry: 1,
            class: PROVIDER.into(),
        },
    );
    let stored = stored(&rig, "j-tr1");
    assert_eq!(stored.request, expected);
    // Task, budget and the spent budget-retry marker survive; unrelated
    // payload keys survive beside the recovery metadata.
    assert_eq!(stored.request.task.as_deref(), Some("t1"));
    assert_eq!(stored.request.budget, 900);
    assert_eq!(
        stored.request.payload["budget_retry"],
        serde_json::json!(true)
    );
    assert_eq!(stored.request.payload["slice"], serde_json::json!(1));
    // The replacement is indexed as the same step at the failed job's own
    // attempt: the transient ordinal never touches work-attempt numbering.
    assert_eq!(
        stored.step_job,
        Some(StepJobRow {
            job: "j-tr1".into(),
            run: 3,
            node: "slice".into(),
            task: "t1".into(),
            attempt: 2,
        })
    );
    assert_eq!(rig.store.last_attempt("t1").expect("attempts"), 2);
    // The replacement is queued for the same task's collector.
    let came_back = rig.st.jobs().get("j-tr1").expect("queued job");
    assert_eq!(came_back.task.as_deref(), Some("t1"));
    assert_eq!(came_back.status, JobStatus::Pending);
}

#[test]
fn deadlines_advance_60_120_240_and_persist_once() {
    let mut rig = rig();
    let (first, d1) = queued(&mut rig, &failed("j", identity()), Some(&bound("j")));
    assert_eq!(first, "j-tr1");
    assert_eq!(d1, T0 + 60 + transient::jitter("j-tr1"));
    let (_, d2) = queued(&mut rig, &chained("j-tr1", "j", 1), Some(&bound("j-tr1")));
    assert_eq!(d2, T0 + 120 + transient::jitter("j-tr2"));
    let (_, d3) = queued(&mut rig, &chained("j-tr2", "j", 2), Some(&bound("j-tr2")));
    assert_eq!(d3, T0 + 240 + transient::jitter("j-tr3"));
    // The deadlines live in the rows as absolute seconds.
    for (id, expected) in [("j-tr1", d1), ("j-tr2", d2), ("j-tr3", d3)] {
        assert_eq!(stored(&rig, id).deadline, expected);
    }
    // A repeated collection of the first failure is answered from the row:
    // the same replacement, the persisted deadline, no new selection.
    let intent = rig
        .store
        .recovery_intent_of_failed_job("j")
        .expect("read")
        .expect("row");
    assert_eq!(
        offer(&mut rig, &failed("j", identity()), Some(&bound("j"))),
        Decision::AlreadyRecorded {
            replacement: intent.replacement_job.clone(),
            deadline: intent.deadline,
        }
    );
}

#[test]
fn three_retries_then_exhausted() {
    let mut rig = rig();
    assert!(matches!(
        offer(&mut rig, &failed("j", identity()), Some(&bound("j"))),
        Decision::Queued { .. }
    ));
    assert!(matches!(
        offer(&mut rig, &chained("j-tr1", "j", 1), Some(&bound("j-tr1"))),
        Decision::Queued { .. }
    ));
    assert!(matches!(
        offer(&mut rig, &chained("j-tr2", "j", 2), Some(&bound("j-tr2"))),
        Decision::Queued { .. }
    ));
    // The fourth failure of the step input finds the allowance spent — as a
    // direct successor or through a correction of the third retry.
    assert_eq!(
        offer(&mut rig, &chained("j-tr3", "j", 3), Some(&bound("j-tr3"))),
        Decision::Exhausted
    );
    assert_eq!(
        offer(
            &mut rig,
            &chained("j-tr3-c1", "j", 3),
            Some(&bound("j-tr3-c1"))
        ),
        Decision::Exhausted
    );
    assert_eq!(
        rig.store
            .pending_recoveries(NEED, "slice")
            .expect("read")
            .len(),
        3
    );
    assert_eq!(rig.store.recovery_allowance("t1", "j").expect("read"), 3);
}

#[test]
fn duplicate_allocation_spends_no_ordinal() {
    let mut rig = rig();
    let job = failed("j", identity());
    let (replacement, deadline) = queued(&mut rig, &job, Some(&bound("j")));
    assert_eq!(
        offer(&mut rig, &job, Some(&bound("j"))),
        Decision::AlreadyRecorded { replacement, deadline }
    );
    assert_eq!(rig.store.recovery_allowance("t1", "j").expect("read"), 1);
    assert_eq!(
        rig.store
            .pending_recoveries(NEED, "slice")
            .expect("read")
            .len(),
        1
    );
}

#[test]
fn interrupted_enqueue_is_restored_from_the_frozen_intent() {
    let mut rig = rig();
    let job = full_job("j");
    let (replacement, deadline) = queued(&mut rig, &job, Some(&bound("j")));
    // The earlier process froze the intent and marked its enqueue complete,
    // but the state save never landed: the replacement job is missing.
    rig.st.jobs_mut().remove(&replacement);
    let intent = stored(&rig, &replacement);
    assert_eq!(intent.deadline, deadline);
    assert_eq!(restore(&mut rig, &intent), Restore::Restored);
    // The restored job is the frozen request, unchanged.
    let came_back = rig.st.jobs().get(&replacement).expect("restored");
    assert_eq!(came_back.body, intent.request.body);
    assert_eq!(came_back.task.as_deref(), Some("t1"));
    assert_eq!(came_back.budget, 900);
    // The deadline was not reset and no second intent appeared.
    assert_eq!(stored(&rig, &replacement).deadline, deadline);
    // Restoring again duplicates nothing, and an intent whose replacement
    // went missing after the applied mark restores just the same.
    assert_eq!(restore(&mut rig, &intent), Restore::Present);
    assert_eq!(rig.st.jobs().len(), 1);
    rig.st.jobs_mut().remove(&replacement);
    assert_eq!(restore(&mut rig, &intent), Restore::Restored);
}

#[test]
fn owned_job_allocates_and_restores_without_a_step_binding() {
    let mut rig = rig();
    // An independently collected owned job: no step-job binding is offered,
    // and its collection ownership is none of the intent's business.
    let job = failed(
        "reslice-t1-a1",
        serde_json::json!({"need": NEED, "run": 3, "task": "t1"}),
    );
    let (replacement, _) = queued(&mut rig, &job, None);
    assert_eq!(replacement, "reslice-t1-a1-tr1");
    let intent = stored(&rig, &replacement);
    assert!(intent.step_job.is_none(), "owned jobs carry no binding");
    assert_eq!(intent.need, NEED);
    assert_eq!(intent.run, 3);
    assert_eq!(intent.task, "t1");
    assert_eq!(
        rig.st.jobs().get(&replacement).expect("queued").task.as_deref(),
        Some("t1")
    );
    // The crash seam works without a binding: validate, then re-enqueue.
    rig.st.jobs_mut().remove(&replacement);
    assert_eq!(restore(&mut rig, &intent), Restore::Restored);
}

#[test]
fn restore_refuses_when_the_flow_moved_on() {
    // The chain advanced: the replacement failed and won ordinal 2, so the
    // ordinal-1 intent is no longer the newest spend of the input.
    let mut rig = rig();
    let (replacement, _) = queued(&mut rig, &failed("j", identity()), Some(&bound("j")));
    rig.st.jobs_mut().remove(&replacement);
    let intent = stored(&rig, &replacement);
    queued(&mut rig, &chained("j-tr1", "j", 1), Some(&bound("j-tr1")));
    assert!(matches!(restore(&mut rig, &intent), Restore::Stale { .. }));
    assert!(
        !rig.st.jobs().contains_key(&replacement),
        "a stale intent restored nothing"
    );
    // A task that is no longer a step owns nothing.
    let mut gone = intent.clone();
    gone.task = String::from("gone");
    assert!(matches!(restore(&mut rig, &gone), Restore::Stale { .. }));

    // A closed step owns no replacements.
    let mut rig = rig();
    let (replacement, _) = queued(&mut rig, &failed("j", identity()), Some(&bound("j")));
    rig.st.jobs_mut().remove(&replacement);
    let intent = stored(&rig, &replacement);
    rig.store
        .set_step_state("t1", StepState::Done, Some("merged"))
        .expect("close step");
    assert!(matches!(restore(&mut rig, &intent), Restore::Stale { .. }));

    // A newer work attempt supersedes the frozen attempt.
    let mut rig = rig();
    let (replacement, _) = queued(&mut rig, &failed("j", identity()), Some(&bound("j")));
    rig.st.jobs_mut().remove(&replacement);
    let intent = stored(&rig, &replacement);
    rig.store
        .insert_step_job(
            NEED,
            &StepJobRow {
                job: "j-a3".into(),
                run: 3,
                node: "slice".into(),
                task: "t1".into(),
                attempt: 3,
            },
        )
        .expect("index newer attempt");
    assert!(matches!(restore(&mut rig, &intent), Restore::Stale { .. }));
}

#[test]
fn autonomous_reruns_and_corrections_cannot_replenish_the_allowance() {
    let mut rig = rig();
    // An autonomous rerun of the first retry carries the input's metadata:
    // its failure spends the SECOND ordinal of input `j` and the
    // replacement continues that chain — never a fresh allowance under its
    // own id, which the metadata overrides.
    let (replacement, _) = queued(
        &mut rig,
        &chained("j-tr1-a2", "j", 1),
        Some(&bound("j-tr1-a2")),
    );
    assert_eq!(replacement, "j-tr2");
    // A budget retry is an ordinary payload carrier: its marker rides along
    // on the minted replacement and buys nothing back.
    let mut budgeted = chained("j-tr2", "j", 2);
    budgeted.payload["budget_retry"] = serde_json::json!(true);
    let (replacement, _) = queued(&mut rig, &budgeted, Some(&bound("j-tr2")));
    assert_eq!(replacement, "j-tr3");
    let minted = stored(&rig, "j-tr3");
    assert_eq!(
        minted.request.payload["budget_retry"],
        serde_json::json!(true)
    );
    // A correction of the third retry inherits the same ordinal: the
    // allowance stays spent.
    assert_eq!(
        offer(
            &mut rig,
            &chained("j-tr3-c1", "j", 3),
            Some(&bound("j-tr3-c1"))
        ),
        Decision::Exhausted
    );
    assert_eq!(rig.store.recovery_allowance("t1", "j").expect("read"), 3);
}

#[test]
fn ineligible_and_unsafe_jobs_get_explicit_decisions() {
    let mut rig = rig();
    // A clean success is ordinary collection's business.
    let mut clean = failed("j", identity());
    clean.outcome = Some(JobOutcome {
        state: "finished".into(),
        detail: "finished push=committed".into(),
        receipt: receipt(0, "answer = \"built\"", ""),
        at: 7,
    });
    assert_eq!(
        offer(&mut rig, &clean, Some(&bound("j"))),
        Decision::NotEligible
    );
    assert!(
        rig.store
            .pending_recoveries(NEED, "slice")
            .expect("read")
            .is_empty()
    );
    // A landed push owns the step: never a rerun, whatever the reply said.
    let mut published = failed("j", identity());
    let outcome = published.outcome.as_mut().expect("outcome");
    outcome.receipt = format!(
        "pool result j: finished\nagent exit 1 in 3s, job 9s\n\
         runner publish: pushed refs/heads/work c1 c1\n\n\
         --- receipt\n\n\n--- stderr tail\n429: rate limit\n\n--- runner log tail\nlog"
    );
    assert!(matches!(
        offer(&mut rig, &published, Some(&bound("j"))),
        Decision::Unsafe { .. }
    ));
    assert!(
        rig.store
            .pending_recoveries(NEED, "slice")
            .expect("read")
            .is_empty()
    );
}
