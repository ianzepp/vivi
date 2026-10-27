//! Tests for durable transient-retry allocation and replay.

use tempfile::{TempDir, tempdir};

use super::{Decision, FIRST_DELAY_SECS, MAX_RETRIES, Replay, allocate, replay, replay_pending};
use crate::pump::config::PumpConfig;
use crate::pump::flow::index::{RunRow, StepJobRow, StepState, run_state};
use crate::pump::flow::recovery::{EMPTY, RecoveryMeta, metadata};
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::jobs::{self, JobOutcome, JobSpec, JobStatus, JobStore, PoolJob};
use crate::pump::state::State;
use crate::pump::store::PumpStore;
use crate::pump::store::recovery::{RecoveryIntentRow, recovery_state};
use crate::pump::transient;

const NEED: &str = "n1";
const RUN_N: u32 = 3;
const NODE: &str = "slice";
const TASK: &str = "t1";
/// The collection instant every allocation in these tests happens at.
const NOW: u64 = 1_000;

/// One mailspace with an open pump database and an empty job map.
struct Rig {
    /// Keeps the project directory alive for the whole test.
    _dir: TempDir,
    ctx: Ctx,
    store: PumpStore,
    st: State,
}

fn rig() -> Rig {
    let dir = tempdir().expect("tempdir");
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join("pump.db")).expect("open store");
    let st = State::load(&dir.path().join("state.json")).expect("load state");
    Rig {
        _dir: dir,
        ctx,
        store,
        st,
    }
}

/// A run of the research flow over [`NEED`], in the given state.
fn run_row(state: &str) -> RunRow {
    RunRow {
        need: NEED.into(),
        run: RUN_N,
        flow: "research".into(),
        flow_version: 1,
        kind: "research".into(),
        entry: "research".into(),
        state: state.into(),
    }
}

/// Index the live run and step rows replay validation reads.
fn indexed(rig: &Rig) {
    rig.store.insert_run(&run_row(run_state::LIVE)).expect("run");
    let step = run_row(run_state::LIVE).step(NODE, TASK, StepState::Running);
    rig.store.insert_step(NEED, &step).expect("step");
}

/// The step-job binding of a failed job running the step's `attempt`.
fn binding(job: &str, attempt: u32) -> StepJobRow {
    StepJobRow {
        job: job.into(),
        run: RUN_N,
        node: NODE.into(),
        task: TASK.into(),
        attempt,
    }
}

/// A finished read-only step job whose reply is genuinely empty: the
/// classifier names it `empty` and the safety gate passes it without git
/// evidence, so these tests exercise the ledger, not the gate.
fn failed_job(id: &str) -> PoolJob {
    let mut spec = JobSpec::new(id, "step", "hand", "subject", "body");
    spec.pool_candidates = vec!["fast".into()];
    spec.local_only = true;
    spec.pool_exclude = vec!["flaky".into()];
    spec.target = Some("vivi".into());
    spec.from_ref = Some("main".into());
    spec.model = Some("m".into());
    spec.builtin_role = Some("hand".into());
    spec.branch = Some("work".into());
    spec.publish_ref = Some("refs/heads/work".into());
    spec.publish_contains = Some("sha0".into());
    spec.budget = 900;
    spec.read_only = true;
    spec.units = vec!["u1".into()];
    spec.source_jobs = vec!["j0".into()];
    spec.payload = serde_json::json!({"budget_retry": true, "attempt": 4, "keep": 1});
    spec.task = Some(TASK.into());
    let mut job = PoolJob::from(spec);
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: "finished".into(),
        detail: String::new(),
        receipt: receipt(),
        at: 7,
    });
    job
}

/// A pool receipt for a clean agent exit with an empty final message.
fn receipt() -> String {
    "pool result j: finished\nagent exit 0 in 3s, job 9s\n\n--- receipt\n\n\n\
     --- stderr tail\n\n\n--- runner log tail\nlog"
        .to_owned()
}

/// Allocate `job` as an indexed step job of [`TASK`] at attempt 4.
fn allocated(rig: &Rig, job: &PoolJob) -> Decision {
    let row = binding(&job.id, 4);
    allocate(
        &rig.ctx,
        &rig.store,
        &run_row(run_state::LIVE),
        Some(&row),
        job,
        NOW,
    )
    .expect("allocate")
}

/// The intent behind a `Queued` decision.
fn queued(decision: Decision) -> RecoveryIntentRow {
    match decision {
        Decision::Queued { intent, .. } => intent,
        other => panic!("expected Queued, got {other:?}"),
    }
}

#[test]
fn transient_retry_preserves_execution_inputs() {
    let rig = rig();

    // A real work failure is never eligible: no transient class, no intent.
    let mut work = failed_job("j-work");
    work.payload = serde_json::json!({});
    work.outcome = Some(JobOutcome {
        state: "finished".into(),
        detail: String::new(),
        receipt: "pool result j: finished\nagent exit 1 in 3s\n\n--- receipt\n\
                  answer = \"failed\"\n\n--- stderr tail\npanicked\n"
            .to_owned(),
        at: 7,
    });
    assert_eq!(allocated(&rig, &work), Decision::NotEligible);

    // A published job is unsafe even though its reply was empty: the
    // privileged-runner receipt owns the step, never a replacement.
    let mut published = failed_job("j-pub");
    published.outcome = Some(JobOutcome {
        state: "finished".into(),
        detail: String::new(),
        receipt: "pool result j: finished\nagent exit 0 in 3s\n\
                  runner publish: pushed refs/heads/work sha0 sha1\n\n\
                  --- receipt\n\n"
            .to_owned(),
        at: 7,
    });
    assert_eq!(
        allocated(&rig, &published),
        Decision::Unsafe {
            code: "publish-receipt"
        }
    );

    // The transient candidate freezes an intent and enqueues nothing.
    let intent = queued(allocated(&rig, &failed_job("j1")));
    assert!(rig.st.jobs().is_empty(), "allocation never enqueues");
    assert_eq!(intent.failed_job, "j1");
    assert_eq!(intent.replacement_job, "j1-tr1");
    assert_eq!(intent.ordinal, 1);
    assert_eq!(intent.reason, EMPTY);
    assert_eq!(intent.state, recovery_state::PENDING);

    // Every execution input survives the rerun_spec copy: identity, full
    // request shape, task, payload keys and the spent budget-retry marker.
    let request = &intent.request;
    assert_eq!(request.id, "j1-tr1");
    assert_eq!(request.kind, "step");
    assert_eq!(request.role, "hand");
    assert_eq!(request.subject, "subject");
    assert_eq!(request.body, "body");
    assert_eq!(request.pool_candidates, vec!["fast".to_owned()]);
    assert!(request.local_only);
    assert_eq!(request.pool_exclude, vec!["flaky".to_owned()]);
    assert_eq!(request.target.as_deref(), Some("vivi"));
    assert_eq!(request.from_ref.as_deref(), Some("main"));
    assert_eq!(request.model.as_deref(), Some("m"));
    assert_eq!(request.builtin_role.as_deref(), Some("hand"));
    assert_eq!(request.branch.as_deref(), Some("work"));
    assert_eq!(request.publish_ref.as_deref(), Some("refs/heads/work"));
    assert_eq!(request.publish_contains.as_deref(), Some("sha0"));
    assert_eq!(request.budget, 900);
    assert!(request.read_only);
    assert_eq!(request.units, vec!["u1".to_owned()]);
    assert_eq!(request.source_jobs, vec!["j0".to_owned()]);
    assert_eq!(request.task.as_deref(), Some(TASK));
    assert_eq!(
        request.payload.get("budget_retry"),
        Some(&serde_json::json!(true))
    );
    assert_eq!(request.payload.get("keep"), Some(&serde_json::json!(1)));
    // The stamped attempt is the failed job's own: the transient ordinal is
    // separate from the work-attempt and correction policies.
    assert_eq!(request.payload.get("attempt"), Some(&serde_json::json!(4)));
    assert_eq!(
        metadata(&request.payload),
        Some(RecoveryMeta {
            input: TASK.into(),
            retry: 1,
            class: EMPTY.into(),
        })
    );

    // The binding keeps the step coordinates and the same attempt.
    let bound = intent.step_job.as_ref().expect("indexed intent");
    assert_eq!(bound.job, "j1-tr1");
    assert_eq!(bound.run, RUN_N);
    assert_eq!(bound.node, NODE);
    assert_eq!(bound.task, TASK);
    assert_eq!(bound.attempt, 4);
}

#[test]
fn deadlines_scale_by_ordinal_and_are_persisted_once() {
    let rig = rig();
    let first = queued(allocated(&rig, &failed_job("j1")));
    let second = queued(allocated(&rig, &failed_job("j1-c1")));
    let third = queued(allocated(&rig, &failed_job("j1-a2")));

    // 60, 120 and 240 seconds plus the deterministic jitter keyed by the
    // replacement id; each is selected once at allocation time.
    assert_eq!(
        first.deadline,
        NOW + FIRST_DELAY_SECS + transient::jitter("j1-tr1")
    );
    assert_eq!(
        second.deadline,
        NOW + FIRST_DELAY_SECS * 2 + transient::jitter("j1-tr2")
    );
    assert_eq!(
        third.deadline,
        NOW + FIRST_DELAY_SECS * 4 + transient::jitter("j1-tr3")
    );

    // A much later collection of the first failed job, against a reopened
    // store, retrieves the stored deadline untouched.
    let reopened = reopen(&rig._dir);
    let again = allocate(
        &rig.ctx,
        &reopened,
        &run_row(run_state::LIVE),
        Some(&binding("j1", 4)),
        &failed_job("j1"),
        NOW + 9_000,
    )
    .expect("allocate");
    assert_eq!(
        again,
        Decision::Queued {
            intent: first,
            recorded: true,
        }
    );
}

/// Reopen the store at `dir` to prove frozen rows survive the round trip.
fn reopen(dir: &TempDir) -> PumpStore {
    PumpStore::open(&dir.path().join("pump.db")).expect("reopen store")
}

#[test]
fn three_retry_cap_and_no_replenishment() {
    let rig = rig();

    // Three distinct failed jobs of one step input spend the allowance.
    let _ = queued(allocated(&rig, &failed_job("j1")));
    let _ = queued(allocated(&rig, &failed_job("j1-c1")));
    let third = queued(allocated(&rig, &failed_job("j1-a2")));
    assert_eq!(third.ordinal, 3);
    assert_eq!(MAX_RETRIES, 3);

    // A fourth candidate — the failed correction of the last retry, an
    // autonomous new attempt, or a budget-retried one, all the same step
    // input — is refused however fresh its failure looks.
    for id in ["j1-tr3-c1", "j1-a3", "j1-c2"] {
        assert_eq!(
            allocated(&rig, &failed_job(id)),
            Decision::Exhausted { spent: 3 },
            "no policy replenishes the allowance ({id})"
        );
    }

    // A distinct step input keeps its own allowance.
    let mut other = failed_job("j9");
    other.task = Some("t9".into());
    let fresh = allocate(
        &rig.ctx,
        &rig.store,
        &run_row(run_state::LIVE),
        Some(&binding("j9", 4)),
        &other,
        NOW,
    )
    .expect("allocate");
    assert_eq!(queued(fresh).ordinal, 1);
}

#[test]
fn duplicate_allocation_retrieves_the_stored_intent() {
    let rig = rig();
    let intent = queued(allocated(&rig, &failed_job("j1")));

    let replayed = allocated(&rig, &failed_job("j1"));
    assert_eq!(
        replayed,
        Decision::Queued {
            intent: intent.clone(),
            recorded: true,
        }
    );
    // No second ordinal was spent and the frozen request is unchanged.
    assert_eq!(
        rig.store
            .recovery_allowance(TASK, TASK)
            .expect("allowance"),
        1
    );
    assert_eq!(
        rig.store
            .recovery_intent("j1-tr1")
            .expect("read")
            .expect("intent"),
        intent
    );
}

#[test]
fn interrupted_enqueue_is_restored_without_reset_or_duplicate() {
    let mut rig = rig();
    indexed(&rig);
    let intent = queued(allocated(&rig, &failed_job("j1")));
    let deadline = intent.deadline;

    // An earlier process minted the replacement and marked the intent
    // applied, then crashed before the State save: the applied stamp
    // survives in the store, the job is gone from the pool.
    jobs::enqueue(&mut rig.st, intent.request.clone()).expect("enqueue");
    rig.store.mark_recovery_applied("j1-tr1", deadline).expect("mark");
    rig.st.jobs_mut().remove("j1-tr1");

    // Replay restores the missing replacement from the frozen request.
    assert_eq!(
        replay(&mut rig.st, &rig.store, &intent, deadline).expect("replay"),
        Replay::Restored {
            replacement: "j1-tr1".into(),
        }
    );
    let job = rig.st.jobs().get("j1-tr1").expect("restored job");
    assert_eq!(job.payload, intent.request.payload);
    assert_eq!(
        rig.store
            .step_job("j1-tr1")
            .expect("read")
            .expect("indexed"),
        intent.step_job.clone().expect("binding")
    );
    let stored = rig
        .store
        .recovery_intent("j1-tr1")
        .expect("read")
        .expect("intent");
    assert_eq!(stored.state, recovery_state::APPLIED);
    assert_eq!(stored.deadline, deadline, "replay never moves a deadline");

    // A second replay is a no-op: present jobs are never duplicated.
    assert_eq!(
        replay(&mut rig.st, &rig.store, &intent, deadline).expect("replay"),
        Replay::Present
    );
    assert_eq!(rig.st.jobs().len(), 1);
}

#[test]
fn replay_waits_for_the_deadline_and_validates_coordinates() {
    let mut rig = rig();
    indexed(&rig);
    let intent = queued(allocated(&rig, &failed_job("j1")));

    // Before the deadline the replacement stays unenqueued.
    assert_eq!(
        replay(&mut rig.st, &rig.store, &intent, NOW).expect("replay"),
        Replay::Waiting
    );
    assert!(rig.st.jobs().is_empty());
    assert_eq!(
        replay_pending(&mut rig.st, &rig.store, NOW).expect("replay pending"),
        vec![(intent.clone(), Replay::Waiting)]
    );

    // At the deadline the pending intent fires: enqueue, index, applied.
    assert_eq!(
        replay_pending(&mut rig.st, &rig.store, intent.deadline).expect("replay pending"),
        vec![(
            intent.clone(),
            Replay::Restored {
                replacement: "j1-tr1".into(),
            }
        )]
    );
    assert!(rig.st.jobs().contains_key("j1-tr1"));

    // A fresh input freezes a second intent, whose replay is refused when
    // its owner run is no longer live.
    let second = queued(allocated(&rig, &failed_job("j2")));
    rig.store
        .set_run_state(NEED, RUN_N, run_state::FINISHING)
        .expect("run state");
    assert_eq!(
        replay(&mut rig.st, &rig.store, &second, second.deadline).expect("replay"),
        Replay::Refused {
            why: "run-not-live"
        }
    );

    // A closed step task refuses its intent too.
    let third = queued(allocated(&rig, &failed_job("j3")));
    rig.store
        .set_step_state(TASK, StepState::Done, Some("merged"))
        .expect("step state");
    assert_eq!(
        replay(&mut rig.st, &rig.store, &third, third.deadline).expect("replay"),
        Replay::Refused {
            why: "task-closed"
        }
    );

    // A newer attempt on the task makes the frozen attempt stale.
    let fourth = queued(allocated(&rig, &failed_job("j4")));
    rig.store
        .insert_step_job(NEED, &binding("j1-a9", 9))
        .expect("newer attempt");
    assert_eq!(
        replay(&mut rig.st, &rig.store, &fourth, fourth.deadline).expect("replay"),
        Replay::Refused {
            why: "attempt-stale"
        }
    );

    // A newer ordinal on the same input supersedes the older intent.
    let fifth = queued(allocated(&rig, &failed_job("j5")));
    let _ = queued(allocated(&rig, &failed_job("j5-c1")));
    assert_eq!(
        replay(&mut rig.st, &rig.store, &fifth, fifth.deadline).expect("replay"),
        Replay::Refused {
            why: "input-superseded"
        }
    );
}

#[test]
fn owned_jobs_allocate_without_a_binding() {
    let mut rig = rig();
    let job = failed_job("reslice-t1-a1");

    // No binding: the owned job keeps its collection ownership — no
    // step-job stamp is added, only the recovery metadata.
    let decision = allocate(
        &rig.ctx,
        &rig.store,
        &run_row(run_state::LIVE),
        None,
        &job,
        NOW,
    )
    .expect("allocate");
    let intent = queued(decision);
    assert!(intent.step_job.is_none());
    assert_eq!(intent.node, String::new());
    assert_eq!(intent.input, TASK);
    assert_eq!(
        intent.request.payload.get("attempt"),
        Some(&serde_json::json!(4))
    );
    assert_eq!(
        metadata(&intent.request.payload),
        Some(RecoveryMeta {
            input: TASK.into(),
            retry: 1,
            class: EMPTY.into(),
        })
    );

    // Replay restores an owned replacement with no index write at all.
    let deadline = intent.deadline;
    assert_eq!(
        replay(&mut rig.st, &rig.store, &intent, deadline).expect("replay"),
        Replay::Restored {
            replacement: "reslice-t1-a1-tr1".into(),
        }
    );
    assert!(rig.st.jobs().contains_key("reslice-t1-a1-tr1"));
    assert_eq!(
        rig.store.step_job("reslice-t1-a1-tr1").expect("read"),
        None,
        "owned jobs stay unindexed"
    );
    // And the input's allowance counts the owned retry like any other.
    assert_eq!(
        rig.store
            .recovery_allowance(TASK, TASK)
            .expect("spent"),
        1
    );
}
