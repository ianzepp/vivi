//! Tests for the durable same-step retry runtime: one replacement on
//! repeated scheduling, fixed redispatch deadlines, unchanged run/node/task
//! under advancing attempts, idempotent materialization, and exhaustion
//! that survives a store reopen.

use std::collections::BTreeMap;

use tempfile::TempDir;

use super::super::Transient;
use super::{
    Decision, EXHAUSTED_KEY, ExhaustedRetry, FailedStep, PendingRetry, exhausted_class,
    materialize, schedule, settle,
};
use crate::error::VivariumError;
use crate::pump::flow::index::{
    FLOW_VERSION_NUMBER, RunRow, StepJobRow, StepRow, StepState, run_state,
};
use crate::pump::jobs::{JobSpec, JobStatus, JobStore, PoolJob};
use crate::pump::store::step_execution::ExecutionKey;
use crate::pump::store::{DB_FILENAME, PumpStore};
use crate::pump::transient;

type R = Result<(), VivariumError>;

const NEED: &str = "n1";
const RUN: u32 = 1;
const NODE: &str = "hand";
const TASK: &str = "t1";
const PHASE: &str = "hand";
/// The clock the first schedule pass of each test runs at.
const NOW: u64 = 1_000;

/// A step mid-flight over a real pump store: run and step indexed, the
/// failed attempt 2 job durable in both the job map and the step-job index,
/// and its generation-1 execution active on it.
struct Rig {
    /// Keeps the project directory alive for the whole test.
    dir: TempDir,
    store: PumpStore,
    jobs: Jobs,
    key: ExecutionKey,
    /// The failed execution: generation 1, active on `job-a2`.
    execution: String,
}

/// An in-memory durable job map over the real enqueue path.
struct Jobs(BTreeMap<String, PoolJob>);

impl JobStore for Jobs {
    fn jobs(&self) -> &BTreeMap<String, PoolJob> {
        &self.0
    }
    fn jobs_mut(&mut self) -> &mut BTreeMap<String, PoolJob> {
        &mut self.0
    }
}

impl Rig {
    fn open() -> Result<Self, VivariumError> {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
        store.insert_run(&RunRow {
            need: NEED.into(),
            run: RUN,
            flow: "slice".into(),
            flow_version: FLOW_VERSION_NUMBER,
            kind: "slice".into(),
            entry: NODE.into(),
            state: run_state::LIVE.into(),
        })?;
        store.insert_step(
            NEED,
            &StepRow {
                run: RUN,
                node: NODE.into(),
                task: TASK.into(),
                state: StepState::Running,
                verdict: None,
            },
        )?;
        let key = ExecutionKey::new(NEED, RUN, TASK, PHASE);
        let generation = store.register_execution(&key)?;
        store.insert_step_job(NEED, &job_row("job-a2", 2))?;
        store.activate_job(&generation.execution, "job-a2", 2)?;
        let jobs = Jobs(
            ["job-a2", "job-b2", "job-c2"]
                .into_iter()
                .map(|id| {
                    (
                        id.into(),
                        PoolJob::from(JobSpec::new(id, "slice", "hand", "subject", "body")),
                    )
                })
                .collect(),
        );
        Ok(Self {
            dir,
            store,
            jobs,
            key,
            execution: generation.execution,
        })
    }

    /// A failed step offer for `job` of generation 1, transient of `class`.
    fn failed(&self, job: &str, attempt: u32, class: &'static str) -> FailedStep {
        FailedStep {
            key: self.key.clone(),
            generation: 1,
            execution: self.execution.clone(),
            job: self.jobs.0[job].clone(),
            row: job_row(job, attempt),
            transient: Some(Transient {
                class,
                evidence: " 502 bad gateway".into(),
            }),
        }
    }

    /// The task's active execution after the retries consumed so far.
    fn active(&self) -> Result<(u32, String), VivariumError> {
        let row = self.store.active_execution(&self.key)?.expect("active");
        Ok((row.generation, row.execution))
    }
}

fn job_row(job: &str, attempt: u32) -> StepJobRow {
    StepJobRow {
        job: job.into(),
        run: RUN,
        node: NODE.into(),
        task: TASK.into(),
        attempt,
    }
}

fn pending(decision: Decision) -> PendingRetry {
    match decision {
        Decision::Pending(pending) => *pending,
        other => panic!("expected a pending replacement, got {other:?}"),
    }
}

fn exhausted(decision: Decision) -> ExhaustedRetry {
    match decision {
        Decision::Exhausted(exhausted) => *exhausted,
        other => panic!("expected exhaustion, got {other:?}"),
    }
}

#[test]
fn repeated_scheduling_reserves_one_replacement() -> R {
    let mut rig = Rig::open()?;
    let failed = rig.failed("job-a2", 2, "provider");
    let first = pending(schedule(&mut rig.jobs, &rig.store, failed, NOW)?);
    assert_eq!(first.intent.replacement_job, "job-a2-r1");
    assert_eq!(first.intent.ordinal, 1);
    assert_eq!(
        first.intent.deadline,
        NOW + 30 + transient::jitter("job-a2-r1")
    );
    assert_eq!(
        first.execution,
        format!("{NEED}:r{RUN}:{TASK}:{PHASE}:g2"),
        "the replacement executes on its own distinct generation"
    );
    let attempt = first.intent.spec.payload["attempt"]
        .as_u64()
        .expect("attempt");
    assert_eq!(attempt, 3, "the attempt advances, the step identity holds");
    assert_eq!(first.intent.spec.payload["node"], NODE);
    assert!(rig.store.execution(&rig.execution)?.expect("g1").superseded);
    assert_eq!(rig.active()?.0, 2);
    assert_eq!(rig.store.retry_intents(&rig.key, 1)?.len(), 1);
    assert!(
        !rig.jobs.0.contains_key("job-a2-r1"),
        "scheduling reserves; only materialization enqueues"
    );

    // the replay answers from the stored intent, never a rewrite or a
    // second generation
    let failed = rig.failed("job-a2", 2, "provider");
    let replay = schedule(&mut rig.jobs, &rig.store, failed, NOW)?;
    assert_eq!(replay, Decision::Pending(Box::new(first)));
    assert_eq!(rig.store.retry_intents(&rig.key, 1)?.len(), 1);
    assert_eq!(rig.active()?.0, 2);
    Ok(())
}

#[test]
fn no_transient_leaves_the_step_entirely_alone() -> R {
    let mut rig = Rig::open()?;
    let mut quiet = rig.failed("job-a2", 2, "provider");
    quiet.transient = None;
    assert_eq!(
        schedule(&mut rig.jobs, &rig.store, quiet, NOW)?,
        Decision::NoTransient
    );
    assert_eq!(rig.store.retry_intents(&rig.key, 1)?.len(), 0);
    assert!(!rig.store.execution(&rig.execution)?.expect("g1").superseded);
    assert_eq!(rig.active()?.0, 1);
    Ok(())
}

#[test]
fn redispatch_deadlines_are_fixed_per_position() -> R {
    let mut rig = Rig::open()?;
    let failed = rig.failed("job-a2", 2, "provider");
    let first = pending(schedule(&mut rig.jobs, &rig.store, failed, NOW)?);
    assert_eq!(
        first.intent.deadline,
        NOW + 30 + transient::jitter("job-a2-r1"),
        "the first redispatch waits 30 seconds plus jitter"
    );
    assert_eq!(first.intent.spec.not_before, first.intent.deadline);

    // the replacement's own transient failure chains -r2 at the second delay
    materialize(&mut rig.jobs, &rig.store, &first, NOW + 40)?;
    let (generation, execution) = rig.active()?;
    let failed = FailedStep {
        key: rig.key.clone(),
        generation,
        execution,
        job: rig.jobs.0["job-a2-r1"].clone(),
        row: rig.store.step_job("job-a2-r1")?.expect("indexed"),
        transient: Some(Transient {
            class: "timeout",
            evidence: "pool timeout".into(),
        }),
    };
    let second = pending(schedule(&mut rig.jobs, &rig.store, failed, NOW + 4_000)?);
    assert_eq!(second.intent.replacement_job, "job-a2-r2");
    assert_eq!(
        second.intent.deadline,
        NOW + 4_000 + 120 + transient::jitter("job-a2-r2"),
        "the second redispatch waits 120 seconds plus jitter"
    );
    assert_eq!(
        second.execution,
        format!("{NEED}:r{RUN}:{TASK}:{PHASE}:g3"),
        "each execution keeps a distinct identity"
    );
    Ok(())
}

#[test]
fn materialize_enqueues_indexes_and_activates_idempotently() -> R {
    let mut rig = Rig::open()?;
    let failed = rig.failed("job-a2", 2, "provider");
    let pending = pending(schedule(&mut rig.jobs, &rig.store, failed, NOW)?);
    materialize(&mut rig.jobs, &rig.store, &pending, 1_040)?;

    // the recorded spec is enqueued verbatim, dispatch hold included
    let job = &rig.jobs.0["job-a2-r1"];
    assert_eq!(job.not_before, pending.intent.deadline);
    assert_eq!(job.body, "body", "no input is rebuilt at materialization");
    // the index keeps run, node and task; only the attempt advanced
    let row = rig.store.step_job("job-a2-r1")?.expect("indexed");
    assert_eq!(
        (row.run, row.node.as_str(), row.task.as_str()),
        (RUN, NODE, TASK)
    );
    assert_eq!(row.attempt, 3);
    // the recorded execution activated the replacement
    let holder = rig.store.execution_by_job("job-a2-r1")?.expect("activated");
    assert_eq!(holder.execution, pending.execution);
    assert_ne!(holder.execution, rig.execution);
    assert_eq!(holder.attempt, Some(3));
    // the step runs again and the intent records its materialization
    assert_eq!(
        rig.store.step_by_task(TASK)?.expect("step").state,
        StepState::Running
    );
    assert_eq!(
        rig.store
            .retry_intent("job-a2-r1")?
            .expect("stored")
            .materialized_at,
        Some(NOW + 40)
    );

    // every repeated pass is a no-op that keeps the first stamp
    materialize(&mut rig.jobs, &rig.store, &pending, 2_000)?;
    assert_eq!(rig.jobs.0["job-a2-r1"].status, JobStatus::Pending);
    assert_eq!(
        rig.store
            .retry_intent("job-a2-r1")?
            .expect("stored")
            .materialized_at,
        Some(NOW + 40)
    );

    // settlement progress is durable and first-stamp
    settle(&rig.store, "job-a2-r1", 3_000)?;
    settle(&rig.store, "job-a2-r1", 4_000)?;
    let settled = rig.store.retry_intent("job-a2-r1")?.expect("stored");
    assert_eq!(settled.settled_at, Some(3_000));
    assert!(!settled.is_incomplete());
    Ok(())
}

#[test]
fn exhaustion_is_durable_and_grants_no_fallback() -> R {
    let mut rig = Rig::open()?;
    let failed_a = rig.failed("job-a2", 2, "provider");
    let first = pending(schedule(&mut rig.jobs, &rig.store, failed_a, NOW)?);
    let failed_b = rig.failed("job-b2", 2, "throttled");
    let second = pending(schedule(&mut rig.jobs, &rig.store, failed_b, NOW)?);
    assert_eq!((first.intent.ordinal, second.intent.ordinal), (1, 2));

    let failed_c = rig.failed("job-c2", 2, "timeout");
    let spent = exhausted(schedule(&mut rig.jobs, &rig.store, failed_c, NOW + 2_000)?);
    assert_eq!(spent.class, "timeout", "the class that spent the last slot");
    assert_eq!(spent.history, vec![first.intent, second.intent]);
    assert_eq!(
        exhausted_class(&rig.jobs.0["job-c2"]),
        Some("timeout"),
        "the failed job carries the durable marker"
    );
    assert!(rig.jobs.0["job-c2"].payload[EXHAUSTED_KEY].is_string());
    assert_eq!(
        rig.store.retry_intents(&rig.key, 1)?.len(),
        2,
        "exhaustion reserves nothing and grants no fallback budget"
    );

    // the budget and the answers survive a reopen, from the stored rows
    let failed_a = rig.failed("job-a2", 2, "provider");
    let failed_c = rig.failed("job-c2", 2, "timeout");
    let key = rig.key.clone();
    let mut jobs = rig.jobs;
    let dir = rig.dir;
    drop(rig.store);
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;

    let replayed = pending(schedule(&mut jobs, &store, failed_a, NOW + 7_000)?);
    assert_eq!(
        replayed.intent.deadline,
        NOW + 30 + transient::jitter("job-a2-r1"),
        "recovery honors the stored deadline over the later clock"
    );
    assert_eq!(
        exhausted(schedule(&mut jobs, &store, failed_c, NOW + 8_000)?)
            .history
            .len(),
        2
    );
    assert_eq!(store.retry_intents(&key, 1)?.len(), 2);
    Ok(())
}
