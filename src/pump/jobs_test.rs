use std::collections::BTreeMap;

use super::{
    JobOutcome, JobSpec, JobStatus, JobStore, PoolJob, apply_reconcile, branch_landed,
    branch_sources_pending, dispatch_order, enqueue, enqueue_promotion, integration_error,
    mark_applied, mark_gated, mark_merged, pool_command, recover_dispatched,
};
use crate::pump::config::PumpConfig;
use crate::pump::host::{FakeHost, test_ctx};
use crate::routing::{RoleRouting, RoutedRole};

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

fn spec(id: &str) -> JobSpec {
    JobSpec::new(id, "planner:board", "planner", "plan board", "write a plan")
}

#[test]
fn enqueue_is_idempotent_but_rejects_reusing_an_id_for_other_work() {
    let mut store = Store::default();
    let job = spec("plan-board-1");
    enqueue(&mut store, job.clone()).unwrap();
    enqueue(&mut store, job).unwrap();
    assert_eq!(store.jobs.len(), 1);

    let mut changed = spec("plan-board-1");
    changed.body = String::from("different plan");
    assert!(enqueue(&mut store, changed).is_err());
}

#[test]
fn enqueue_rejects_path_unsafe_job_ids() {
    let mut store = Store::default();
    assert!(enqueue(&mut store, spec("../outside")).is_err());
    assert!(store.jobs.is_empty());
}

#[test]
fn recovery_and_fix_jobs_dispatch_before_ordinary_cadence_jobs() {
    let mut gate = PoolJob::from(JobSpec::new("gate", "gate", "gate", "gate", "run gate"));
    gate.queued_at = 20;
    let mut fix_planner = PoolJob::from(JobSpec::new(
        "plan-pumpfix-g1",
        "planner:board",
        "planner",
        "fix plan",
        "plan",
    ));
    fix_planner.payload = serde_json::json!({"need":"pumpfix-g1"});
    let mut fix_merge = PoolJob::from(JobSpec::new(
        "merge-fix",
        "merge",
        "merge",
        "fix merge",
        "merge",
    ));
    fix_merge.units = vec![String::from("pumpfix-g1-1")];
    let mut priority_plan = PoolJob::from(JobSpec::new(
        "plan-hot",
        "planner:board",
        "planner",
        "priority plan",
        "plan",
    ));
    priority_plan.payload = serde_json::json!({"need":"hot"});
    let cadence = PoolJob::from(JobSpec::new(
        "cadence-a",
        "cadence",
        "cadence",
        "routine run",
        "run",
    ));
    let mut jobs = vec![cadence, priority_plan, fix_merge, fix_planner, gate];
    let priorities = BTreeMap::from([(String::from("hot"), 3)]);
    jobs.sort_by_key(|job| dispatch_order(job, "pumpfix", &priorities));

    let ids: Vec<&str> = jobs.iter().map(|job| job.id.as_str()).collect();
    assert_eq!(ids[0], "gate");
    assert!(ids[1..3].contains(&"plan-pumpfix-g1"));
    assert!(ids[1..3].contains(&"merge-fix"));
    assert_eq!(ids[3], "plan-hot");
    assert_eq!(ids[4], "cadence-a");
}

#[test]
fn recovery_finds_a_task_created_before_the_last_state_save() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    host.reply(
        "task list",
        0,
        r#"[{"handle":"a1b2c3d4","subject":"[pump-job:plan-board-1] plan board"}]"#,
    );
    host.reply(
        "task show a1b2c3d4 --json",
        0,
        r#"{"messages":[{"body":"pump-job-id: plan-board-1"}]}"#,
    );
    host.reply(
        "task show a1b2c3d4 --latest",
        0,
        "pool dispatch 123\npool local  slot 1  model m\nfrom main",
    );
    let mut store = Store::default();
    enqueue(&mut store, spec("plan-board-1")).unwrap();

    recover_dispatched(&ctx, &mut store).unwrap();

    let job = &store.jobs["plan-board-1"];
    assert_eq!(job.task.as_deref(), Some("a1b2c3d4"));
    assert_eq!(job.pool.as_deref(), Some("local"));
    assert_eq!(job.status, JobStatus::Dispatched);
    assert!(
        !host
            .commands()
            .iter()
            .any(|command| command.contains("task send"))
    );
}

#[test]
fn recovery_records_a_result_that_landed_before_the_last_state_save() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    host.reply(
        "task show a1b2c3d4 --latest",
        0,
        "pool result 3: finished\npool local slot 1\npush: pushed next abcdef0123\nagent exit 0 in 2s",
    );
    let mut store = Store::default();
    let mut job = PoolJob::from(spec("plan-board-1"));
    job.task = Some(String::from("a1b2c3d4"));
    job.status = JobStatus::Dispatched;
    store.jobs.insert(job.id.clone(), job);

    recover_dispatched(&ctx, &mut store).unwrap();

    let job = &store.jobs["plan-board-1"];
    assert_eq!(job.status, JobStatus::Finished);
    assert_eq!(job.outcome.as_ref().unwrap().agent_success(), Some(true));
}

#[test]
fn finished_write_job_with_nonzero_exit_never_enters_the_merge_queue() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    host.reply(
        "task show a1b2c3d4 --latest",
        0,
        "pool result 3: finished\npush: committed cadence/role-a1 abcdef0123\nagent exit 124 after 90s",
    );
    let mut store = Store::default();
    let mut spec = JobSpec::new("cadence-role-a1", "cadence", "cadence", "run", "brief");
    spec.target = Some(String::from("default"));
    spec.branch = Some(String::from("cadence/role-a1"));
    spec.integration_required = true;
    let mut job = PoolJob::from(spec);
    job.task = Some(String::from("a1b2c3d4"));
    job.status = JobStatus::Dispatched;
    store.jobs.insert(job.id.clone(), job);
    let seen = BTreeMap::from([(
        String::from("a1b2c3d4"),
        (
            String::from("finished"),
            String::from("committed cadence/role-a1 abcdef0123"),
        ),
    )]);

    apply_reconcile(&ctx, &mut store, &seen);

    assert!(integration_error(&store, "cadence-role-a1").is_some());
    assert!(branch_sources_pending(&store, "default").is_empty());
}

#[test]
fn write_role_job_is_not_applied_until_its_merge_and_gate_receipts() {
    let mut store = Store::default();
    let mut spec = JobSpec::new("cadence-role-a1", "cadence", "cadence", "run", "brief");
    spec.integration_required = true;
    let mut job = PoolJob::from(spec);
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: String::from("finished"),
        detail: String::from("pushed cadence/role-a1 0123456789"),
        receipt: String::from("agent exit 0 in 2s"),
        at: 10,
    });
    store.jobs.insert(job.id.clone(), job);

    assert!(!branch_landed(&store, "cadence-role-a1"));
    assert!(!mark_applied(&mut store, "cadence-role-a1", 11));
    mark_merged(
        &mut store,
        &[String::from("cadence-role-a1")],
        "0123456789012345678901234567890123456789",
    );
    assert!(!branch_landed(&store, "cadence-role-a1"));
    mark_gated(
        &mut store,
        &[String::from("cadence-role-a1")],
        "0123456789012345678901234567890123456789",
    );
    assert!(branch_landed(&store, "cadence-role-a1"));
    assert!(mark_applied(&mut store, "cadence-role-a1", 12));
}

#[test]
fn pool_command_carries_repository_and_write_context() {
    let mut job = PoolJob::from(spec("merge-1"));
    job.target = Some(String::from("default"));
    job.from_ref = Some(String::from("origin/next"));
    job.branch = Some(String::from("next"));
    job.budget = 45;
    job.read_only = true;
    job.with = vec![String::from("unit/u1-a1")];
    job.model = Some(String::from("model/a"));
    job.allow_default_branch = true;
    job.publish_ref = Some(String::from("refs/heads/next"));
    job.publish_expect = Some(String::from("a".repeat(40)));
    let command = pool_command(&job, "local", "a1b2c3d4");
    assert!(command.contains("--pool 'local'"));
    assert!(command.contains("a1b2c3d4"));
    assert!(command.contains("--target 'default'"));
    assert!(command.contains("--from 'origin/next'"));
    assert!(command.contains("--branch 'next'"));
    assert!(command.contains("--with 'unit/u1-a1'"));
    assert!(command.contains("--model 'model/a'"));
    assert!(command.contains("--allow-default-branch"));
    assert!(command.contains("--budget 45"));
    assert!(command.contains("--read-only"));
    assert!(command.contains("--publish-ref 'refs/heads/next'"));
    assert!(command.contains(&format!("--publish-expect '{}'", "a".repeat(40))));
}

#[test]
fn promotion_job_binds_an_exact_green_candidate_to_main_on_a_scratch_branch() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = PumpConfig::default();
    config.branch = String::from("next");
    config.main = String::from("main");
    let (ctx, _) = test_ctx(dir.path(), config);
    let ctx = ctx.with_routing(RoleRouting {
        roles: vec![RoutedRole {
            name: String::from("release"),
            kind: Some(String::from("release")),
            status: String::from("active"),
            pool: Some(String::from("local")),
            read_only: Some(false),
            model: None,
            models: Vec::new(),
        }],
        ..RoleRouting::default()
    });
    let mut store = Store::default();
    let tip = "0123456789012345678901234567890123456789";
    let id = enqueue_promotion(&ctx, &mut store, "default", tip, &[])
        .unwrap()
        .unwrap();
    let job = &store.jobs[&id];

    assert_eq!(job.publish_ref.as_deref(), Some("refs/heads/main"));
    assert_eq!(job.publish_expect.as_deref(), Some(tip));
    assert_eq!(job.from_ref.as_deref(), Some(tip));
    assert_ne!(job.branch.as_deref(), Some("main"));
    assert!(!job.allow_default_branch);
    assert!(job.body.contains("Do not push"));
}

#[test]
fn outcome_reports_success_only_when_agent_exit_is_explicit() {
    let outcome = JobOutcome {
        state: String::from("finished"),
        detail: String::from("pushed branch sha"),
        receipt: String::from("agent exit 0 in 8s"),
        at: 42,
    };
    assert_eq!(outcome.agent_success(), Some(true));
    let failed = JobOutcome {
        receipt: String::from("agent exit 2 in 8s"),
        ..outcome.clone()
    };
    assert_eq!(failed.agent_success(), Some(false));
    let unknown = JobOutcome {
        receipt: String::from("no exit receipt"),
        ..outcome
    };
    assert_eq!(unknown.agent_success(), None);
}
