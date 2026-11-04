//! `rerun_spec` copies every execution input and drops runtime result state.

use std::collections::BTreeMap;

use crate::pump::jobs::{JobOutcome, JobSpec, JobStatus, JobStore, PoolJob, enqueue};

#[derive(Default)]
struct Store {
    jobs: BTreeMap<String, PoolJob>,
}

impl JobStore for Store {
    fn jobs(&self) -> &BTreeMap<String, PoolJob> {
        &self.jobs
    }

    fn jobs_mut(&mut self) -> &mut BTreeMap<String, PoolJob> {
        &mut self.jobs
    }
}

/// A job with every execution input set to a distinct value and every
/// runtime ownership/result field also set.
fn dispatched_job(id: &str) -> PoolJob {
    let mut spec = JobSpec::new(id, "planner", "pump-planner", "subject", "body");
    spec.pool_candidates = vec!["alpha".into(), "beta".into()];
    spec.local_only = true;
    spec.pool_exclude = vec!["refused".into()];
    spec.target = Some("target-repo".into());
    spec.extra_targets = vec!["side-repo".into()];
    spec.from_ref = Some("refs/heads/from".into());
    spec.with = vec!["refs/heads/extra".into()];
    spec.model = Some("model-x".into());
    spec.command = Some("run.sh".into());
    spec.builtin_role = Some("tugboat".into());
    spec.branch = Some("vivi/work/x".into());
    spec.allow_default_branch = true;
    spec.publish_ref = Some("refs/heads/publish".into());
    spec.publish_expect = Some("expect-sha".into());
    spec.publish_contains = Some("contain-sha".into());
    spec.delete_ref = Some("refs/heads/delete".into());
    spec.delete_expect = Some("delete-sha".into());
    spec.budget = 900;
    spec.read_only = true;
    spec.units = vec!["unit-1".into()];
    spec.source_jobs = vec!["src-1".into()];
    spec.payload = serde_json::json!({"custom": 7});
    spec.task = Some("task-1".into());
    let mut job = PoolJob::from(spec);
    job.queued_at = 100;
    job.pool = Some("alpha".into());
    job.dispatched_at = Some(200);
    job.status = JobStatus::Dispatched;
    job.outcome = Some(JobOutcome {
        state: "finished".into(),
        detail: "ok".into(),
        receipt: "receipt text".into(),
        at: 300,
    });
    job.applied_at = Some(400);
    job.dispatch_refusals = 2;
    job.held = Some("too many refusals".into());
    job
}

#[test]
fn rerun_spec_copies_every_execution_input() {
    let job = dispatched_job("job-1");
    let spec = job.rerun_spec("job-1-a2");

    let expected = JobSpec {
        id: "job-1-a2".into(),
        kind: job.kind.clone(),
        role: job.role.clone(),
        subject: job.subject.clone(),
        body: job.body.clone(),
        pool_candidates: vec!["alpha".into(), "beta".into()],
        local_only: true,
        pool_exclude: vec!["refused".into()],
        target: Some("target-repo".into()),
        extra_targets: vec!["side-repo".into()],
        from_ref: Some("refs/heads/from".into()),
        with: vec!["refs/heads/extra".into()],
        model: Some("model-x".into()),
        command: Some("run.sh".into()),
        builtin_role: Some("tugboat".into()),
        branch: Some("vivi/work/x".into()),
        allow_default_branch: true,
        publish_ref: Some("refs/heads/publish".into()),
        publish_expect: Some("expect-sha".into()),
        publish_contains: Some("contain-sha".into()),
        delete_ref: Some("refs/heads/delete".into()),
        delete_expect: Some("delete-sha".into()),
        budget: 900,
        read_only: true,
        units: vec!["unit-1".into()],
        source_jobs: vec!["src-1".into()],
        payload: serde_json::json!({"custom": 7}),
        task: Some("task-1".into()),
    };
    assert_eq!(spec, expected);
}

#[test]
fn enqueueing_a_rerun_spec_starts_fresh_runtime_state() {
    let job = dispatched_job("job-1");
    let spec = job.rerun_spec("job-1-a2");

    let mut store = Store::default();
    enqueue(&mut store, spec).expect("enqueue rerun spec");
    let queued = &store.jobs["job-1-a2"];
    assert_eq!(queued.status, JobStatus::Pending);
    assert_eq!(queued.pool, None);
    assert_eq!(queued.dispatched_at, None);
    assert_eq!(queued.outcome, None);
    assert_eq!(queued.applied_at, None);
    assert_eq!(queued.dispatch_refusals, 0);
    assert_eq!(queued.held, None);
    assert_eq!(queued.task.as_deref(), Some("task-1"));
}

#[test]
fn rerun_spec_changes_only_the_identity() {
    let job = dispatched_job("job-1");
    let original = job.rerun_spec("job-1-a2");
    let again = job.rerun_spec("job-1-a3");
    assert_ne!(original, again);
    assert_eq!(original.id, "job-1-a2");
    assert_eq!(again.id, "job-1-a3");
}
