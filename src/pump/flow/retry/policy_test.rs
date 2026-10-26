//! Tests for the shared bounded-retry policy: the allowance survives
//! reloads and alternating causes, and a persisted intent is recovered
//! idempotently after partial writes.

use std::path::Path;

use super::{RetryOutcome, RetryStep, retry};
use crate::error::VivariumError;
use crate::pump::flow::index::{FLOW_VERSION_NUMBER, RunRow, StepJobRow, run_state};
use crate::pump::flow::retry::{EMPTY_REPLY, PROVIDER, RUNNER};
use crate::pump::jobs::{self, JobOutcome, JobSpec, JobStatus, PoolJob, replay};
use crate::pump::state::State;
use crate::pump::store::flow_retry::{FlowRetryDecision, FlowRetryRequest, RETRY_LIMIT};
use crate::pump::store::{DB_FILENAME, PumpStore};
use crate::pump::transient;
use tempfile::tempdir;

type R = Result<(), VivariumError>;

/// The fixture need.
const NEED: &str = "n1";
/// The fixture run number.
const RUN: u32 = 1;
/// The fixture node.
const NODE: &str = "slice:1";
/// The fixture task.
const TASK: &str = "t1";
/// The execution budget every fixture job carries.
const BUDGET: u64 = 600;
/// The clock the first failure is collected at.
const NOW: u64 = 1_000;

/// The fixture step coordinates.
fn step() -> RetryStep<'static> {
    RetryStep {
        need: NEED,
        run: RUN,
        node: NODE,
        task: TASK,
    }
}

/// A step-job row of the fixture step.
fn row(job: &str, attempt: u32) -> StepJobRow {
    StepJobRow {
        job: job.into(),
        run: RUN,
        node: NODE.into(),
        task: TASK.into(),
        attempt,
    }
}

/// A pool receipt: the summary line, an optional stderr tail and the hand's
/// own final message.
fn receipt(exit: i32, stderr: &str, message: &str) -> String {
    let mut out = format!("agent exit {exit} in 9s\n");
    if !stderr.is_empty() {
        out.push_str(&format!("\n--- stderr tail\n{stderr}\n"));
    }
    out.push_str(&format!("\n--- receipt\n{message}\n"));
    out
}

/// Finish `job` the way the pool writes a clean terminal outcome.
fn finish(job: &mut PoolJob, detail: &str, receipt_text: &str) {
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: "finished".into(),
        detail: detail.into(),
        receipt: receipt_text.into(),
        at: NOW,
    });
}

/// Provider evidence: a rate limit in the stderr tail.
fn fail_provider(job: &mut PoolJob) {
    finish(job, "", &receipt(1, "provider api returned HTTP 429", ""));
}

/// Runner evidence: a failed fetch named by the outcome detail.
fn fail_runner(job: &mut PoolJob) {
    finish(job, "fetch_failed exited 128", &receipt(0, "", ""));
}

/// A clean exit that left no substantive reply.
fn fail_empty(job: &mut PoolJob) {
    finish(job, "", &receipt(0, "", ""));
}

/// Ordinary failing test output: never a retry.
fn fail_ordinary(job: &mut PoolJob) {
    finish(
        job,
        "",
        &receipt(1, "assertion failed: left == right", "tried twice"),
    );
}

/// A queued step-job spec of the fixture step at `attempt` and input
/// `generation`.
fn new_spec(id: &str, attempt: u32, generation: u32) -> JobSpec {
    let mut spec = JobSpec::new(id, "slice", "hand", "subject", "body");
    spec.budget = BUDGET;
    spec.payload = serde_json::json!({});
    row(id, attempt).stamp(NEED, &mut spec.payload);
    replay::replace_input(&mut spec.payload, u64::from(generation));
    spec
}

/// Queue `id` as a step job of the fixture step, finish it with `cause`,
/// and record its index row.
fn send(
    st: &mut State,
    store: &PumpStore,
    id: &str,
    attempt: u32,
    generation: u32,
    cause: fn(&mut PoolJob),
) -> R {
    jobs::enqueue(st, new_spec(id, attempt, generation))?;
    cause(st.jobs.get_mut(id).expect("queued job"));
    store.insert_step_job(NEED, &row(id, attempt))
}

/// Fail job `id` in place with `cause` and drive the policy over it at
/// `now`.
fn collect(
    state: &mut State,
    store: &PumpStore,
    id: &str,
    cause: fn(&mut PoolJob),
    now: u64,
) -> Result<RetryOutcome, VivariumError> {
    cause(state.jobs.get_mut(id).expect("failed job"));
    let failed = job_of(state, id);
    let row = store.step_job(id)?.expect("index row of the failed job");
    retry(state, store, &failed, &step(), Some(&row), now)
}

/// The job with `id`.
fn job_of(state: &State, id: &str) -> PoolJob {
    state.jobs.get(id).cloned().expect("job present")
}

/// The decision of a pending replacement.
fn pending(outcome: RetryOutcome) -> FlowRetryDecision {
    match outcome {
        RetryOutcome::ReplacementPending(decision) => decision,
        other => panic!("expected a pending replacement; actual: {other:?}"),
    }
}

/// The history of an exhausted allowance.
fn exhausted(outcome: RetryOutcome) -> Vec<FlowRetryDecision> {
    match outcome {
        RetryOutcome::Exhausted { history } => history,
        other => panic!("expected an exhausted allowance; actual: {other:?}"),
    }
}

/// A live run of the fixture need.
fn start_run(store: &PumpStore) -> R {
    store.insert_run(&RunRow {
        need: NEED.into(),
        run: RUN,
        flow: "build".into(),
        flow_version: FLOW_VERSION_NUMBER,
        kind: "build".into(),
        entry: "plan".into(),
        state: run_state::LIVE.into(),
    })
}

/// Save `state` through the store, drop the database and load both back:
/// the round trip a pump restart gives every durable record.
fn reload(path: &Path, state: State) -> Result<(PumpStore, State), VivariumError> {
    let mut store = PumpStore::open(path)?;
    store.save(&state)?;
    drop(store);
    let store = PumpStore::open(path)?;
    let state = store.load()?;
    Ok((store, state))
}

/// The replacement `id` of `replaces` carries its input unchanged: the same
/// attempt, generation, step identity and budget.
fn assert_replacement(
    state: &State,
    store: &PumpStore,
    id: &str,
    replaces: &str,
    attempt: u32,
    generation: u32,
) -> R {
    let job = job_of(state, id);
    assert_eq!(
        replay::replacement_predecessor(&job.payload),
        Some(replaces)
    );
    assert_eq!(
        replay::input_generation(&job.payload),
        u64::from(generation)
    );
    assert_eq!(job.budget, BUDGET);
    assert_eq!(job.payload["need"], NEED);
    assert_eq!(job.payload["run"], RUN);
    assert_eq!(job.payload["node"], NODE);
    assert_eq!(job.payload["task"], TASK);
    assert_eq!(job.payload["attempt"], attempt);
    let record = store.step_job(id)?.expect("index row");
    assert_eq!((record.run, record.attempt), (RUN, attempt));
    assert_eq!((record.node.as_str(), record.task.as_str()), (NODE, TASK));
    Ok(())
}

/// The fixed backoff of `delay` seconds plus the id's jitter: the
/// replacement must not dispatch before it, and its deadline is one budget
/// later.
fn assert_backoff(decision: &FlowRetryDecision, job: &PoolJob, now: u64, delay: u64) {
    let not_before = replay::dispatch_not_before(&job.payload).expect("dispatch hold");
    assert_eq!(not_before, now + delay + transient::jitter(&job.id));
    assert_eq!(decision.deadline, not_before + BUDGET);
}

/// A decision a crashed first pass could have left behind: the intent, and
/// nothing else. `deadline` is arbitrary on purpose — recovery must reuse
/// whatever the decision says.
fn intent_request(
    failed_job: &str,
    generation: u32,
    deadline: u64,
    replacement: Option<&str>,
) -> FlowRetryRequest {
    FlowRetryRequest {
        need: NEED.into(),
        run: RUN,
        node: NODE.into(),
        task: TASK.into(),
        generation,
        failed_job: failed_job.into(),
        failure_class: PROVIDER.into(),
        replacement_job: replacement.map(str::to_owned),
        deadline,
    }
}

#[test]
fn transient_budget_survives_reload_and_mixed_failures() -> R {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join(DB_FILENAME);
    let store = PumpStore::open(&path).expect("open");
    start_run(&store)?;
    let mut state = State::default();
    send(&mut state, &store, "step-x", 3, 7, fail_provider)?;

    let first = pending(collect(&mut state, &store, "step-x", fail_provider, NOW)?);
    assert_eq!(first.ordinal, 1);
    assert_eq!(first.failure_class, PROVIDER);
    assert_replacement(&state, &store, "step-x-r1", "step-x", 3, 7)?;
    assert_backoff(&first, &job_of(&state, "step-x-r1"), NOW, 30);

    // A pump restart: snapshots and decisions survive the round trip.
    let (store, mut state) = reload(&path, state)?;
    assert_eq!(store.flow_retry_decision("step-x")?, Some(first.clone()));

    let second = pending(collect(
        &mut state,
        &store,
        "step-x-r1",
        fail_runner,
        NOW + 31,
    )?);
    assert_eq!(second.ordinal, 2);
    assert_eq!(second.failure_class, RUNNER);
    assert_backoff(&second, &job_of(&state, "step-x-r2"), NOW + 31, 60);

    let third = pending(collect(
        &mut state,
        &store,
        "step-x-r2",
        fail_empty,
        NOW + 92,
    )?);
    assert_eq!(third.ordinal, 3);
    assert_eq!(third.failure_class, EMPTY_REPLY);
    assert_backoff(&third, &job_of(&state, "step-x-r3"), NOW + 92, 120);

    // Another restart, then a fourth cause: the shared allowance is spent,
    // one replacement was minted per allowed failure and none after.
    let (store, mut state) = reload(&path, state)?;
    let history = exhausted(collect(
        &mut state,
        &store,
        "step-x-r3",
        fail_provider,
        NOW + 300,
    )?);
    assert_eq!(history.len(), 4);
    assert!(!history[3].allowed);
    assert_eq!(history[3].ordinal, RETRY_LIMIT + 1);
    let causes: Vec<&str> = history.iter().map(|d| d.failure_class.as_str()).collect();
    assert_eq!(causes, [PROVIDER, RUNNER, EMPTY_REPLY, PROVIDER]);
    assert!(!state.jobs.contains_key("step-x-r4"));
    assert_eq!(state.jobs.len(), 4);
    assert_replacement(&state, &store, "step-x-r3", "step-x-r2", 3, 7)
}

#[test]
fn retry_intent_is_idempotent_after_partial_writes() -> R {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join(DB_FILENAME);
    let store = PumpStore::open(&path).expect("open");
    start_run(&store)?;
    let mut state = State::default();
    let y_row = row("step-y", 1);
    send(&mut state, &store, "step-y", 1, 2, fail_provider)?;

    // The intent is persisted and nothing else: the policy recovers the
    // enqueue, the identity write and the index row from the decision
    // alone, at a much later clock.
    store.decide_flow_retry(&intent_request("step-y", 2, 5_000, None), NOW - 900)?;
    let (mut store, mut state) = reload(&path, state)?;
    let recovered = pending(collect(&mut state, &store, "step-y", fail_provider, NOW)?);
    assert_eq!(recovered.deadline, 5_000);
    assert_eq!(recovered.ordinal, 1);
    assert_eq!(recovered.generation, 2);
    assert_eq!(recovered.replacement_job.as_deref(), Some("step-y-r1"));
    assert_replacement(&state, &store, "step-y-r1", "step-y", 1, 2)?;
    let job = job_of(&state, "step-y-r1");
    assert_eq!(
        replay::dispatch_not_before(&job.payload),
        Some(5_000 - BUDGET)
    );

    // The intent names its replacement but the enqueue and index row are
    // still missing: the policy enqueues the named identity at the stored
    // deadline, second in the shared allowance.
    send(&mut state, &store, "step-z", 4, 2, fail_runner)?;
    store.decide_flow_retry(
        &intent_request("step-z", 2, 7_000, Some("step-z-r1")),
        NOW - 800,
    )?;
    let named = pending(collect(&mut state, &store, "step-z", fail_runner, NOW + 1)?);
    assert_eq!(named.deadline, 7_000);
    assert_eq!(named.ordinal, 2);
    assert_replacement(&state, &store, "step-z-r1", "step-z", 4, 2)?;
    let job = job_of(&state, "step-z-r1");
    assert_eq!(
        replay::dispatch_not_before(&job.payload),
        Some(7_000 - BUDGET)
    );

    // A fully written intent, re-collected at any later clock, reuses its
    // replacement identity, ordinal, deadline and generation and writes
    // nothing new.
    let jobs_before = state.jobs.len();
    let again = pending(collect(
        &mut state,
        &store,
        "step-y",
        fail_provider,
        NOW + 500,
    )?);
    assert_eq!(again, recovered);
    assert_eq!(state.jobs.len(), jobs_before);
    assert_replacement(&state, &store, "step-y-r1", "step-y", 1, 2)
}

#[test]
fn an_ordinary_failure_or_a_dead_run_stays_unretried() -> R {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    start_run(&store)?;
    let mut state = State::default();
    send(&mut state, &store, "step-o", 1, 1, fail_ordinary)?;

    let outcome = collect(&mut state, &store, "step-o", fail_ordinary, NOW)?;
    assert_eq!(outcome, RetryOutcome::NotRetryable);
    assert_eq!(state.jobs.len(), 1);
    assert_eq!(store.flow_retry_decision("step-o")?, None);

    // A withdrawn or superseded run never receives new work, whatever the
    // evidence says, and no budget is spent on it.
    store.set_run_state(NEED, RUN, run_state::WITHDRAWN)?;
    let outcome = collect(&mut state, &store, "step-o", fail_provider, NOW + 1)?;
    assert_eq!(outcome, RetryOutcome::NotRetryable);
    store.set_run_state(NEED, RUN, run_state::SUPERSEDED)?;
    let outcome = collect(&mut state, &store, "step-o", fail_runner, NOW + 2)?;
    assert_eq!(outcome, RetryOutcome::NotRetryable);
    assert_eq!(state.jobs.len(), 1);
    assert_eq!(store.flow_retry_decision("step-o")?, None);
    Ok(())
}
