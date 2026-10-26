//! Companion tests for pump jobs runtime execution metadata.

use std::collections::BTreeMap;

use super::*;
use crate::pump::jobs::{JobOutcome, JobSpec, JobStore, PoolJob, enqueue};

#[derive(Default)]
struct Store {
    jobs: BTreeMap<String, PoolJob>,
    limits: ProviderLimits,
}

impl JobStore for Store {
    fn jobs(&self) -> &BTreeMap<String, PoolJob> {
        &self.jobs
    }

    fn jobs_mut(&mut self) -> &mut BTreeMap<String, PoolJob> {
        &mut self.jobs
    }
}

impl DispatchStore for Store {
    fn provider_limits(&self) -> &ProviderLimits {
        &self.limits
    }

    fn provider_limits_mut(&mut self) -> &mut ProviderLimits {
        &mut self.limits
    }
}

fn spec(id: &str) -> JobSpec {
    JobSpec::new(id, "planner:board", "planner", "plan board", "write a plan")
}

/// Fully populated runtime metadata with every field at a distinct value.
fn sample_runtime() -> Runtime {
    let mut runtime = Runtime::default();
    runtime.execution_id = Some(String::from("exec-2"));
    runtime.pool = Some(String::from("alpha"));
    runtime.model = Some(String::from("model-x"));
    runtime.provider = Some(String::from("proxy-a"));
    runtime.start_ref = Some(String::from("refs/heads/vivi/work/x"));
    runtime.start_commit = Some("c".repeat(40));
    runtime.started_at = Some(1_700);
    runtime.evidence = Some(ExecutionEvidence::Finished {
        detail: String::from("agent_rc 0"),
        at: 1_800,
    });
    runtime.processed = Some(ProcessedResult {
        execution_id: String::from("exec-2"),
        outcome_digest: String::from("digest-2"),
        processed_at: Some(1_900),
    });
    runtime.rate_limit = Some(RateLimitState {
        retry_count: 1,
        retry_deadline: Some(2_000),
    });
    runtime.scheduled_wait = Some(ScheduledWait {
        scheduled_at: Some(1_950),
        wake_at: 2_000,
    });
    runtime.archived.push(ArchivedExecution {
        execution_id: String::from("exec-1"),
        pool: Some(String::from("alpha")),
        model: None,
        provider: Some(String::from("proxy-a")),
        task: Some(String::from("task0001")),
        started_at: Some(1_000),
        limited_at: Some(1_050),
        outcome: Some(ExecutionEvidence::Timeout {
            detail: String::from("budget exceeded"),
            at: 1_100,
        }),
        receipt: String::from("receipt-1"),
    });
    runtime.continuation = Some(Continuation {
        from_ref: String::from("refs/heads/cont"),
        from_commit: String::from("d").repeat(40),
        recorded_at: Some(1_750),
    });
    runtime
}

#[test]
fn legacy_job_rows_deserialize_with_a_defaulted_runtime() {
    let mut job = PoolJob::from(spec("legacy-1"));
    job.runtime = sample_runtime();
    let mut json = serde_json::to_value(&job).unwrap();
    assert!(json.get("runtime").is_some());
    // A row written before runtime metadata existed carries no runtime key.
    json.as_object_mut()
        .expect("job serializes to an object")
        .remove("runtime");
    let restored: PoolJob = serde_json::from_value(json).unwrap();
    assert_eq!(restored.runtime, Runtime::default());
    assert!(restored.runtime.is_default());
    let mut expected = job;
    expected.runtime = Runtime::default();
    assert_eq!(restored, expected);
}

#[test]
fn changing_runtime_metadata_does_not_invalidate_identical_enqueue() {
    let mut store = Store::default();
    enqueue(&mut store, spec("plan-board-1")).unwrap();
    store.jobs.get_mut("plan-board-1").unwrap().runtime = sample_runtime();
    // The same spec still reconciles to the existing job, runtime and all.
    enqueue(&mut store, spec("plan-board-1")).unwrap();
    assert_eq!(store.jobs.len(), 1);
    assert_eq!(store.jobs["plan-board-1"].runtime, sample_runtime());

    let mut other = spec("plan-board-1");
    other.body = String::from("different plan");
    assert!(enqueue(&mut store, other).is_err());
}

#[test]
fn a_default_runtime_is_invisible_in_serialization_and_round_trips() {
    let mut job = PoolJob::from(spec("round-1"));
    assert!(serde_json::to_value(&job).unwrap().get("runtime").is_none());
    job.runtime = sample_runtime();
    let json = serde_json::to_value(&job).unwrap();
    assert!(json.get("runtime").is_some());
    let restored: PoolJob = serde_json::from_value(json).unwrap();
    assert_eq!(restored, job);
}

#[test]
fn archived_rate_limited_executions_restore_their_terminal_result() {
    let mut runtime = Runtime::default();
    runtime.archived.push(ArchivedExecution {
        execution_id: String::from("exec-0"),
        pool: Some(String::from("alpha")),
        model: Some(String::from("model-x")),
        provider: Some(String::from("proxy-a")),
        task: Some(String::from("task0001")),
        started_at: Some(10),
        limited_at: Some(11),
        outcome: Some(ExecutionEvidence::Lost {
            detail: String::from("lost slot"),
            at: 12,
        }),
        receipt: String::from("receipt-0"),
    });
    let archived = &runtime.archived[0];
    assert_eq!(
        archived.restore_outcome().unwrap(),
        JobOutcome {
            state: String::from("lost"),
            detail: String::from("lost slot"),
            receipt: String::from("receipt-0"),
            at: 12,
        }
    );
    // An archive without a terminal outcome has nothing to restore.
    let mut unfinished = Runtime::default();
    unfinished.archived.push(ArchivedExecution::default());
    assert!(unfinished.archived[0].restore_outcome().is_none());
}

#[test]
fn provider_limit_backoff_steps_saturate_and_cap_the_history() {
    let mut limit = ProviderLimit::default();
    assert_eq!(limit.next_backoff(8, 100), 8);
    for expected in [8, 16, 32] {
        limit.step_backoff(8, 100, 3);
        assert_eq!(limit.backoff_history.last(), Some(&expected));
    }
    assert_eq!(limit.backoff_history, vec![8, 16, 32]);
    for _ in 0..4 {
        limit.step_backoff(8, 100, 3);
    }
    assert_eq!(limit.backoff_history, vec![100, 100, 100]);
    assert_eq!(limit.next_backoff(8, 100), 100);
}

#[test]
fn provider_limits_nest_per_pool_and_per_provider() {
    let mut limits = ProviderLimits::default();
    assert!(limits.pool("alpha").is_none());
    assert!(limits.provider("proxy-a").is_none());
    limits.pool_mut("alpha").cooldown_until = Some(90);
    limits.provider_mut("proxy-a").effective_limit = Some(30);
    assert_eq!(limits.pool("alpha").unwrap().cooldown_until, Some(90));
    assert_eq!(
        limits.provider("proxy-a").unwrap().effective_limit,
        Some(30)
    );
}

#[test]
fn dispatch_store_exposes_provider_limits_over_a_job_store() {
    let mut store = Store::default();
    enqueue(&mut store, spec("plan-board-1")).unwrap();
    store.provider_limits_mut().pool_mut("alpha").cooldown_until = Some(90);
    assert_eq!(
        store
            .provider_limits()
            .pool("alpha")
            .unwrap()
            .cooldown_until,
        Some(90)
    );
    assert_eq!(store.jobs().len(), 1);
}
