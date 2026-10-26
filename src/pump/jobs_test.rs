use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use super::{
    JobOutcome, JobSpec, JobStatus, JobStore, PoolJob, candidate_pools, command_for,
    dispatch_order, dispatch_pending, enqueue, mark_applied, pool_command, recover_dispatched,
    release_held_after_reload, send_task,
};
use crate::pump::config::{PumpConfig, PumpTarget};
use crate::pump::fixture::ProbeHost;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::store::{DB_FILENAME, PumpStore};
use crate::routing::{RoleRouting, RoutedPool, RoutedRole};

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
fn enqueue_refuses_a_read_only_job_without_a_start_ref() {
    let mut store = Store::default();
    let mut job = spec("review-1");
    job.read_only = true;
    let err = enqueue(&mut store, job.clone()).unwrap_err().to_string();
    assert!(
        err.contains("review-1") && err.contains("from_ref"),
        "{err}"
    );
    assert!(store.jobs.is_empty());
    job.from_ref = Some("main".into());
    enqueue(&mut store, job).unwrap();
    assert_eq!(store.jobs.len(), 1);
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
fn non_flow_jobs_rank_by_priority_then_age() {
    let mut priority_plan = PoolJob::from(JobSpec::new(
        "plan-hot",
        "planner:board",
        "planner",
        "priority plan",
        "plan",
    ));
    priority_plan.payload = serde_json::json!({"need":"hot"});
    priority_plan.queued_at = 30;
    let mut old = PoolJob::from(JobSpec::new(
        "cadence-a",
        "cadence",
        "cadence",
        "old",
        "run",
    ));
    old.queued_at = 10;
    let mut young = PoolJob::from(JobSpec::new(
        "cadence-b",
        "cadence",
        "cadence",
        "new",
        "run",
    ));
    young.queued_at = 20;
    let mut jobs = vec![young, old, priority_plan];
    let priorities = BTreeMap::from([(String::from("hot"), 3)]);
    jobs.sort_by_key(|job| dispatch_order(job, &priorities));

    let ids: Vec<&str> = jobs.iter().map(|job| job.id.as_str()).collect();
    assert_eq!(ids, ["plan-hot", "cadence-a", "cadence-b"]);
}

fn flow_job(id: &str, kind: &str, class: &str, need: &str, queued_at: u64) -> PoolJob {
    let mut job = PoolJob::from(JobSpec::new(id, "merge", "merge", "s", "b"));
    job.payload = serde_json::json!({"kind": kind, "class": class, "need": need});
    job.queued_at = queued_at;
    job
}

#[test]
fn flow_jobs_rank_repair_then_cto_then_priority_then_age() {
    let priorities = BTreeMap::from([(String::from("hot"), 3)]);
    let mut jobs = vec![
        flow_job("old-plain", "build", "hand", "cold", 1),
        flow_job("hot-build", "build", "hand", "hot", 9),
        flow_job("cto", "build", "cto", "cold", 5),
        flow_job("repair", "repair", "hand", "cold", 7),
    ];
    jobs.sort_by_key(|job| dispatch_order(job, &priorities));
    let ids: Vec<&str> = jobs.iter().map(|job| job.id.as_str()).collect();
    assert_eq!(ids, ["repair", "cto", "hot-build", "old-plain"]);
}

#[test]
fn a_non_flow_job_ranks_behind_repair_and_cto_flow_jobs() {
    let other = PoolJob::from(JobSpec::new("merge-old", "merge", "merge", "s", "b"));
    let priorities = BTreeMap::new();
    let mut jobs = vec![
        other,
        flow_job("cto", "build", "cto", "cold", 99),
        flow_job("repair", "repair", "hand", "cold", 99),
    ];
    jobs.sort_by_key(|job| dispatch_order(job, &priorities));
    let ids: Vec<&str> = jobs.iter().map(|job| job.id.as_str()).collect();
    assert_eq!(ids, ["repair", "cto", "merge-old"]);
}

#[test]
fn one_merge_main_per_target_is_offered_while_another_is_in_flight() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let ctx = ctx.with_routing(merge_routing(None));
    host.reply("pool status", 0, "slot primary bridge slot-1 free\n");
    host.reply("task send", 0, "created hand a1b2c3d4\n");
    host.reply(
        "pool dispatch",
        0,
        "dispatched a1b2c3d4 bridge slot-1 model/default unit/task\n",
    );
    let mut store = Store::default();
    let mut flying = flow_job("mm-flying", "build", "merge-main", "n1", 1);
    flying.target = Some(String::from("t"));
    flying.status = JobStatus::Dispatched;
    let mut blocked = flow_job("mm-t", "build", "merge-main", "n2", 2);
    blocked.target = Some(String::from("t"));
    let mut open = flow_job("mm-u", "build", "merge-main", "n3", 3);
    open.target = Some(String::from("u"));
    for job in [flying, blocked, open] {
        store.jobs.insert(job.id.clone(), job);
    }

    assert_eq!(
        dispatch_pending(&ctx, &mut store).unwrap(),
        vec![String::from("mm-u")]
    );
    assert_eq!(store.jobs["mm-t"].status, JobStatus::Pending);
}

#[test]
fn role_pool_and_explicit_candidates_expand_as_primary_then_fallbacks() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, _) = test_ctx(dir.path(), PumpConfig::default());
    let pool = |fallback: Option<&str>| RoutedPool {
        slots: 1,
        provider: String::from("local"),
        fallback: fallback.map(str::to_owned),
        caps: BTreeMap::new(),
        models: Vec::new(),
        has_bands: false,
    };
    let ctx = ctx.with_routing(RoleRouting {
        roles: vec![RoutedRole {
            name: String::from("gate"),
            kind: Some(String::from("gate")),
            status: String::from("active"),
            pool: Some(String::from("role")),
            read_only: Some(true),
            model: None,
            models: Vec::new(),
        }],
        pools: BTreeMap::from([
            (String::from("role"), pool(Some("role-fallback"))),
            (String::from("role-fallback"), pool(None)),
            (String::from("declared"), pool(Some("declared-fallback"))),
            (String::from("declared-fallback"), pool(None)),
        ]),
    });

    assert_eq!(
        candidate_pools(&ctx, "gate", &[String::from("declared")], false, &[]),
        ["role", "role-fallback", "declared", "declared-fallback"]
    );
}

#[test]
fn durable_merge_dispatch_uses_the_configured_pool_fallback_when_primary_is_full() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let ctx = ctx.with_routing(merge_routing(Some("backup")));
    host.reply(
        "pool status",
        0,
        "slot primary bridge slot-1 occupied\nslot backup bridge slot-1 free\n",
    );
    host.reply("task send", 0, "created hand a1b2c3d4\n");
    host.reply(
        "pool dispatch",
        0,
        "dispatched a1b2c3d4 bridge slot-1 model/default unit/task\n",
    );
    let mut store = Store::default();
    enqueue(
        &mut store,
        JobSpec::new(
            "merge-fallback",
            "merge",
            "merge",
            "integrate",
            "merge branches",
        ),
    )
    .unwrap();

    assert_eq!(
        dispatch_pending(&ctx, &mut store).unwrap(),
        vec![String::from("merge-fallback")]
    );

    let job = &store.jobs["merge-fallback"];
    assert_eq!(job.status, JobStatus::Dispatched);
    assert_eq!(job.pool.as_deref(), Some("backup"));
    assert_eq!(job.task.as_deref(), Some("a1b2c3d4"));
    assert!(
        host.commands().iter().any(|command| {
            command.contains("pool dispatch") && command.contains("--pool backup")
        })
    );
}

#[test]
fn durable_merge_without_a_configured_fallback_stays_queued_without_sending_a_task() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let ctx = ctx.with_routing(merge_routing(None));
    host.reply(
        "pool status",
        0,
        "slot primary bridge slot-1 occupied\nslot unrelated bridge slot-1 free\n",
    );
    let mut store = Store::default();
    enqueue(
        &mut store,
        JobSpec::new(
            "merge-no-fallback",
            "merge",
            "merge",
            "integrate",
            "merge branches",
        ),
    )
    .unwrap();

    assert!(dispatch_pending(&ctx, &mut store).unwrap().is_empty());

    let job = &store.jobs["merge-no-fallback"];
    assert_eq!(job.status, JobStatus::Pending);
    assert_eq!(job.task, None);
    assert_eq!(job.dispatched_at, None);
    assert!(
        !host
            .commands()
            .iter()
            .any(|command| { command.contains("task send") || command.contains("pool dispatch") })
    );
}

fn merge_routing(fallback: Option<&str>) -> RoleRouting {
    let pool = |fallback: Option<&str>| RoutedPool {
        slots: 1,
        provider: String::from("local"),
        fallback: fallback.map(str::to_owned),
        caps: BTreeMap::new(),
        models: Vec::new(),
        has_bands: false,
    };
    let mut pools = BTreeMap::from([(String::from("primary"), pool(fallback))]);
    if fallback.is_some() {
        pools.insert(String::from("backup"), pool(None));
    }
    RoleRouting {
        roles: vec![RoutedRole {
            name: String::from("merge"),
            kind: Some(String::from("merge")),
            status: String::from("active"),
            pool: Some(String::from("primary")),
            read_only: Some(false),
            model: None,
            models: Vec::new(),
        }],
        pools,
    }
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
        &serde_json::json!({
            "messages": [{
                "body": crate::letter::render(
                    &crate::letter::work::TaskFront {
                        job: Some(crate::letter::work::JobFront {
                            id: String::from("plan-board-1"),
                            kind: String::from("planner:board"),
                            builtin_role: None,
                        }),
                        ..Default::default()
                    },
                    "write a plan",
                )
                .unwrap(),
            }],
        })
        .to_string(),
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
fn write_role_job_is_applied_once_when_it_finishes() {
    let mut store = Store::default();
    let spec = JobSpec::new("cadence-role-a1", "cadence", "cadence", "run", "brief");
    let mut job = PoolJob::from(spec);
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: String::from("finished"),
        detail: String::from("pushed cadence/role-a1 0123456789"),
        receipt: String::from("agent exit 0 in 2s"),
        at: 10,
    });
    store.jobs.insert(job.id.clone(), job);

    assert!(mark_applied(&mut store, "cadence-role-a1", 11));
    assert!(!mark_applied(&mut store, "cadence-role-a1", 12));
    assert_eq!(store.jobs["cadence-role-a1"].applied_at, Some(11));
}

#[test]
fn pool_command_carries_repository_and_write_context() {
    let mut job = PoolJob::from(spec("merge-1"));
    job.target = Some(String::from("default"));
    job.from_ref = Some(String::from("origin/next"));
    job.branch = Some(String::from("next"));
    job.budget = 45;
    job.read_only = false;
    job.with = vec![String::from("unit/u1-a1")];
    job.model = Some(String::from("model/a"));
    job.allow_default_branch = true;
    job.publish_ref = Some(String::from("refs/heads/next"));
    job.publish_expect = Some(String::from("a".repeat(40)));
    let command = pool_command(&job, "local", "a1b2c3d4");
    assert!(command.contains("--pool local"));
    assert!(command.contains("a1b2c3d4"));
    assert!(command.contains("--target default"));
    assert!(command.contains("--from origin/next"));
    assert!(command.contains("--branch next"));
    assert!(command.contains("--with unit/u1-a1"));
    assert!(command.contains("--model model/a"));
    assert!(command.contains("--allow-default-branch"));
    assert!(command.contains("--budget 45"));
    assert!(!command.contains("--read-only"));
    assert!(command.contains("--publish-ref refs/heads/next"));
    assert!(command.contains(&format!("--publish-expect {}", "a".repeat(40))));
}

#[test]
fn pool_command_preserves_a_long_repair_budget() {
    let mut spec = JobSpec::new(
        "repair-1",
        "repair",
        "repairer",
        "repair branch",
        "undo the drift on the branch",
    );
    spec.budget = 7200;
    spec.target = Some(String::from("default"));
    spec.branch = Some(String::from("repair/repair-1"));
    let job = PoolJob::from(spec);
    assert_eq!(job.budget, 7200);
    let command = pool_command(&job, "local", "a1b2c3d4");
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let mut budgets = tokens
        .windows(2)
        .filter(|pair| pair[0] == "--budget")
        .map(|pair| pair[1]);
    assert_eq!(budgets.next(), Some("7200"));
    assert_eq!(budgets.next(), None);
}

#[test]
fn pool_command_names_the_job_role_only_when_it_has_one() {
    let mut job = PoolJob::from(spec("merge-2"));
    job.role = String::from("merge");
    assert!(pool_command(&job, "local", "a1b2c3d4").contains("--role merge"));
    job.role = String::new();
    assert!(!pool_command(&job, "local", "a1b2c3d4").contains("--role"));
}

#[test]
fn pool_command_names_the_need_only_when_the_payload_has_one() {
    let mut job = PoolJob::from(spec("step-1"));
    job.payload = serde_json::json!({ "need": "4c01ea79", "run": 1 });
    assert!(pool_command(&job, "local", "a1b2c3d4").contains("--need 4c01ea79"));
    job.payload = serde_json::json!({ "run": 1 });
    assert!(!pool_command(&job, "local", "a1b2c3d4").contains("--need"));
    job.payload = serde_json::json!({ "need": "" });
    assert!(!pool_command(&job, "local", "a1b2c3d4").contains("--need"));
    job.payload = serde_json::Value::Null;
    assert!(!pool_command(&job, "local", "a1b2c3d4").contains("--need"));
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

#[test]
fn an_oversized_brief_is_cut_to_fit_the_letter_cap_and_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    host.reply("task send", 0, "created hand a1b2c3d4\n");
    let body = format!(
        "head of the brief\n{}\nPool result contract: return the plan between the markers.",
        "thread line\n".repeat(20_000)
    );
    let job = queued("planner-big", &body);

    let task = send_task(&ctx, &job).unwrap();

    assert_eq!(task.as_deref(), Some("a1b2c3d4"));
    let sent = std::fs::read_to_string(ctx.home.join("job-briefs/planner-big.txt")).unwrap();
    assert!(sent.len() < 64 * 1024, "{} bytes", sent.len());
    assert!(sent.contains("head of the brief"));
    assert!(sent.contains("brief truncated from"));
    assert!(sent.ends_with("return the plan between the markers."));
    let full = std::fs::read_to_string(ctx.home.join("job-briefs/planner-big.full.txt")).unwrap();
    assert!(full.ends_with(&body));
    let exceptions = std::fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert_eq!(
        exceptions
            .matches("job-brief-truncated planner-big")
            .count(),
        1
    );
}

#[test]
fn a_brief_under_the_cap_is_sent_whole() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    host.reply("task send", 0, "created hand a1b2c3d4\n");
    let job = queued("planner-small", "a short brief");

    send_task(&ctx, &job).unwrap();

    let sent = std::fs::read_to_string(ctx.home.join("job-briefs/planner-small.txt")).unwrap();
    assert!(sent.ends_with("a short brief"));
    assert!(!ctx.home.join("job-briefs/planner-small.full.txt").exists());
    assert!(!ctx.home.join("exceptions.txt").exists());
}

#[test]
fn a_sent_job_brief_carries_the_job_front_matter() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    host.reply("task send", 0, "created hand a1b2c3d4\n");
    let mut spec = JobSpec::new("audit-1", "auditor:range", "auditor", "audit", "look at it");
    spec.builtin_role = Some(String::from("auditor"));
    let mut store = Store::default();
    enqueue(&mut store, spec).unwrap();
    let job = store.jobs["audit-1"].clone();

    send_task(&ctx, &job).unwrap();

    let sent = std::fs::read_to_string(ctx.home.join("job-briefs/audit-1.txt")).unwrap();
    let envelope = crate::letter::read::task_of("", &sent).unwrap();
    let front = envelope.front.job.unwrap();
    assert_eq!(front.id, "audit-1");
    assert_eq!(front.kind, "auditor:range");
    assert_eq!(front.builtin_role.as_deref(), Some("auditor"));
    assert!(!sent.contains("pump-job-"));
    assert!(sent.ends_with("look at it"));
}

fn queued(id: &str, body: &str) -> PoolJob {
    let mut store = Store::default();
    enqueue(
        &mut store,
        JobSpec::new(id, "planner:revision", "planner", "revise", body),
    )
    .unwrap();
    store.jobs[id].clone()
}

/// Three queued merge jobs against a primary pool with two free slots and a
/// backup with one, dispatched through a [`ProbeHost`] with `parallel`.
/// Jobs `m1..m3` already hold tasks `t1..t3`, so each `pool dispatch` line
/// names its job's task.
fn probed_jobs(
    parallel: u32,
    configure: impl FnOnce(&mut ProbeHost),
) -> (Store, Vec<String>, Arc<ProbeHost>, Arc<FakeHost>) {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = PumpConfig::default();
    cfg.dispatch.parallel = parallel;
    let (ctx, host) = test_ctx(dir.path(), cfg);
    let mut probe = ProbeHost::new(&host);
    configure(&mut probe);
    let probe = Arc::new(probe);
    let ctx = Ctx {
        host: probe.clone(),
        ..ctx.with_routing(merge_routing(Some("backup")))
    };
    host.reply(
        "pool status",
        0,
        "slot primary bridge slot-1 free\nslot primary bridge slot-2 free\nslot backup bridge slot-1 free\n",
    );
    host.reply(
        "pool dispatch",
        0,
        "dispatched t bridge slot-1 model/default unit/task\n",
    );
    let mut store = Store::default();
    for n in 1..=3u64 {
        let id = format!("m{n}");
        enqueue(
            &mut store,
            JobSpec::new(&id, "merge", "merge", "integrate", "merge branches"),
        )
        .unwrap();
        let job = store.jobs.get_mut(&id).unwrap();
        job.queued_at = n;
        job.task = Some(format!("t{n}"));
    }
    let started = dispatch_pending(&ctx, &mut store).unwrap();
    (store, started, probe, host)
}

fn pool_of(store: &Store, id: &str) -> Option<String> {
    store.jobs[id].pool.clone()
}

#[test]
fn parallel_one_dispatches_jobs_one_after_another_on_the_tick_thread() {
    let (store, started, probe, _host) = probed_jobs(1, |_| {});
    assert_eq!(started, ["m1", "m2", "m3"]);
    assert_eq!(pool_of(&store, "m1").as_deref(), Some("primary"));
    assert_eq!(pool_of(&store, "m2").as_deref(), Some("primary"));
    assert_eq!(pool_of(&store, "m3").as_deref(), Some("backup"));
    assert_eq!(probe.peak.load(Ordering::SeqCst), 1);
    assert!(
        probe
            .threads
            .lock()
            .unwrap()
            .iter()
            .all(|t| *t == std::thread::current().id())
    );
}

#[test]
fn parallel_jobs_are_inside_pool_dispatch_together_and_land_as_a_serial_run_would() {
    let (store, started, probe, host) = probed_jobs(3, |p| p.want = 3);
    assert_eq!(probe.peak.load(Ordering::SeqCst), 3, "all three at once");
    assert_eq!(started, ["m1", "m2", "m3"], "reported in job order");
    for id in ["m1", "m2", "m3"] {
        assert_eq!(store.jobs[id].status, JobStatus::Dispatched, "{id}");
        assert!(store.jobs[id].dispatched_at.is_some(), "{id}");
    }
    assert_eq!(pool_of(&store, "m1").as_deref(), Some("primary"));
    assert_eq!(pool_of(&store, "m2").as_deref(), Some("primary"));
    assert_eq!(pool_of(&store, "m3").as_deref(), Some("backup"));
    let logged: Vec<String> = host
        .logs()
        .into_iter()
        .filter(|line| line.starts_with("job dispatched"))
        .collect();
    assert!(logged[0].contains("m1") && logged[1].contains("m2") && logged[2].contains("m3"));
}

#[test]
fn jobs_beyond_the_parallel_bound_go_in_a_later_group() {
    let (store, started, probe, _host) = probed_jobs(2, |p| p.want = 2);
    assert_eq!(started, ["m1", "m2", "m3"]);
    assert_eq!(probe.peak.load(Ordering::SeqCst), 2);
    assert_eq!(pool_of(&store, "m3").as_deref(), Some("backup"));
}

#[test]
fn a_job_whose_pool_refuses_it_is_deferred_and_the_others_still_dispatch() {
    for parallel in [1, 3] {
        let (store, started, _probe, host) = probed_jobs(parallel, |p| {
            p.fail = Some(String::from("t2"));
        });
        assert_eq!(started, ["m1", "m3"], "parallel {parallel}");
        assert_eq!(store.jobs["m1"].status, JobStatus::Dispatched);
        assert_eq!(store.jobs["m3"].status, JobStatus::Dispatched);
        let refused = &store.jobs["m2"];
        assert_eq!(refused.status, JobStatus::Pending, "parallel {parallel}");
        assert_eq!(refused.task.as_deref(), Some("t2"), "the task is kept");
        assert!(
            host.logs()
                .iter()
                .any(|line| line.starts_with("job dispatch deferred m2 on ")),
            "parallel {parallel}"
        );
    }
}

#[test]
fn a_job_refused_by_its_first_pool_tries_the_next_candidate() {
    let (store, started, _probe, _host) = probed_jobs(1, |p| {
        // Only the primary pool refuses t1.
        p.fail = Some(String::from("--pool primary t1"));
    });
    assert!(started.contains(&String::from("m1")), "{started:?}");
    assert_eq!(store.jobs["m1"].status, JobStatus::Dispatched);
    assert_eq!(pool_of(&store, "m1").as_deref(), Some("backup"));
}

#[test]
fn a_job_with_a_preset_task_dispatches_without_a_task_send() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let ctx = ctx.with_routing(merge_routing(None));
    host.reply("pool status", 0, "slot primary bridge slot-1 free\n");
    host.reply(
        "pool dispatch",
        0,
        "dispatched a1b2c3d4 bridge slot-1 model/default unit/task\n",
    );
    let mut store = Store::default();
    let mut job = JobSpec::new("step-hand-1", "merge", "merge", "slice", "brief");
    job.task = Some(String::from("a1b2c3d4"));
    enqueue(&mut store, job.clone()).unwrap();
    assert_eq!(store.jobs["step-hand-1"].task.as_deref(), Some("a1b2c3d4"));

    assert_eq!(
        dispatch_pending(&ctx, &mut store).unwrap(),
        vec![String::from("step-hand-1")]
    );

    let stored = &store.jobs["step-hand-1"];
    assert_eq!(stored.status, JobStatus::Dispatched);
    assert_eq!(stored.task.as_deref(), Some("a1b2c3d4"));
    let commands = host.commands();
    assert!(!commands.iter().any(|c| c.contains("task send")));
    assert!(
        commands
            .iter()
            .any(|c| c.contains("pool dispatch") && c.contains("a1b2c3d4"))
    );

    // Re-enqueue is idempotent, and a different preset task is other work.
    enqueue(&mut store, job.clone()).unwrap();
    job.task = Some(String::from("ffffffff"));
    assert!(enqueue(&mut store, job).is_err());
}

#[test]
fn a_spec_without_a_task_still_matches_its_dispatched_job() {
    let mut store = Store::default();
    let job = spec("plan-board-2");
    enqueue(&mut store, job.clone()).unwrap();
    store.jobs.get_mut("plan-board-2").unwrap().task = Some(String::from("a1b2c3d4"));
    enqueue(&mut store, job).unwrap();
}

/// Dispatch one `merge` job (with `task` preset or not) through a fake host
/// whose `pool status` shows one free slot. Returns the commands it ran and
/// the text of the brief reply file, when one was written.
fn dispatch_brief_job(
    task: Option<&str>,
    configure: impl FnOnce(&FakeHost),
) -> (Vec<String>, Option<String>) {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let ctx = ctx.with_routing(merge_routing(None));
    host.reply("pool status", 0, "slot primary bridge slot-1 free\n");
    host.reply(
        "pool dispatch",
        0,
        "dispatched t bridge slot-1 model/default unit/task\n",
    );
    host.reply("task send", 0, "created hand a1b2c3d4\n");
    configure(&host);
    let mut store = Store::default();
    let mut spec = JobSpec::new(
        "h1",
        "merge",
        "merge",
        "unit",
        "WRITE SCOPE: a.rs\nenvelope",
    );
    spec.task = task.map(str::to_owned);
    enqueue(&mut store, spec).unwrap();
    dispatch_pending(&ctx, &mut store).unwrap();
    assert_eq!(store.jobs["h1"].status, JobStatus::Dispatched);
    let brief = std::fs::read_to_string(ctx.home.join("board/reply-h1-brief.txt")).ok();
    (host.commands(), brief)
}

fn replies_in(commands: &[String]) -> usize {
    commands
        .iter()
        .filter(|command| command.contains("mail reply"))
        .count()
}

#[test]
fn a_preset_task_job_posts_one_brief_before_it_dispatches() {
    let (commands, brief) = dispatch_brief_job(Some("t1"), |_| {});
    assert_eq!(replies_in(&commands), 1, "{commands:?}");
    let reply = commands
        .iter()
        .position(|command| command.contains("mail reply"))
        .unwrap();
    let dispatch = commands
        .iter()
        .position(|command| command.contains("pool dispatch"))
        .unwrap();
    assert!(reply < dispatch, "{commands:?}");
    assert!(commands[reply].contains("t1"), "{commands:?}");
    let brief = brief.expect("brief file written");
    let note = crate::mailspace::pump_note::note_of(&brief).expect("a brief note");
    assert_eq!(note.pump.note, crate::letter::records::NoteWord::Brief);
    assert_eq!(note.job.expect("a [job] table").id, "h1");
    assert!(note.prose.contains("WRITE SCOPE: a.rs"), "{brief}");
}

#[test]
fn a_task_that_already_has_this_jobs_brief_gets_no_second_one() {
    let brief = crate::mailspace::pump_note::brief_body(
        crate::letter::work::JobFront {
            id: "h1".into(),
            kind: "merge".into(),
            builtin_role: None,
        },
        "WRITE SCOPE",
    )
    .unwrap();
    let thread = serde_json::json!({"messages": [{"body": brief}]}).to_string();
    let (commands, _) = dispatch_brief_job(Some("t1"), |host| {
        host.reply("mail thread", 0, &thread);
    });
    assert_eq!(replies_in(&commands), 0, "{commands:?}");
    assert!(commands.iter().any(|c| c.contains("pool dispatch")));
}

#[test]
fn a_job_without_a_preset_task_posts_no_brief_reply() {
    let (commands, brief) = dispatch_brief_job(None, |_| {});
    assert_eq!(replies_in(&commands), 0, "{commands:?}");
    assert!(brief.is_none());
    assert!(commands.iter().any(|c| c.contains("task send")));
}

/// One queued `merge` job `m1` with its task `t1` preset, against a pool with
/// one free slot whose `pool dispatch` answers `code`/`reply`.
fn refusal_rig(code: i32, reply: &str) -> (tempfile::TempDir, Ctx, Arc<FakeHost>, Store) {
    refusal_rig_for(Some("t1"), None, code, reply)
}

/// Like [`refusal_rig`], with the job's preset task and the step task its
/// payload carries (a flow job stamps it) chosen by the caller.
fn refusal_rig_for(
    task: Option<&str>,
    step: Option<&str>,
    code: i32,
    reply: &str,
) -> (tempfile::TempDir, Ctx, Arc<FakeHost>, Store) {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let ctx = ctx.with_routing(merge_routing(None));
    host.reply("pool status", 0, "slot primary bridge slot-1 free\n");
    host.reply("pool dispatch", code, reply);
    host.reply("task send", 0, "created merge a1b2c3d4\n");
    let mut store = Store::default();
    let mut job = JobSpec::new("m1", "merge", "merge", "integrate", "merge branches");
    job.task = task.map(str::to_owned);
    if let Some(step) = step {
        job.payload = serde_json::json!({ "task": step });
    }
    enqueue(&mut store, job).unwrap();
    (dir, ctx, host, store)
}

fn pool_dispatch_calls(host: &FakeHost) -> usize {
    host.commands()
        .iter()
        .filter(|c| c.contains("pool dispatch"))
        .count()
}

#[test]
fn three_porcelain_refusals_hold_the_job_and_raise_one_alarm() {
    let (_dir, ctx, host, mut store) = refusal_rig_for(
        Some("t1"),
        Some("t-step"),
        1,
        "refused t1 \"no usable bridge\"\nremote refused\n",
    );
    for tick in 1..=2 {
        dispatch_pending(&ctx, &mut store).unwrap();
        assert_eq!(store.jobs["m1"].dispatch_refusals, tick);
        assert!(store.jobs["m1"].held.is_none(), "tick {tick}");
    }
    assert!(!ctx.home.join("exceptions.txt").exists());

    assert!(dispatch_pending(&ctx, &mut store).unwrap().is_empty());
    let job = &store.jobs["m1"];
    assert_eq!(job.status, JobStatus::Pending);
    assert_eq!(job.dispatch_refusals, 3);
    assert_eq!(job.held.as_deref(), Some("no usable bridge"));
    let exceptions = std::fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert_eq!(exceptions.matches("job-dispatch-refused m1").count(), 1);
    assert!(
        exceptions.contains("refused job m1 3 times: no usable bridge")
            && exceptions.contains("`vivi pump requeue t-step`"),
        "{exceptions}"
    );

    // A held job is not offered to a pool again, and the alarm is not repeated.
    let calls = pool_dispatch_calls(&host);
    assert_eq!(calls, 3);
    dispatch_pending(&ctx, &mut store).unwrap();
    assert_eq!(pool_dispatch_calls(&host), calls);
    let exceptions = std::fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert_eq!(exceptions.matches("job-dispatch-refused m1").count(), 1);
}

#[test]
fn only_the_porcelain_refused_line_counts_toward_a_hold() {
    let (_dir, ctx, host, mut store) = refusal_rig(1, "remote refused\n");
    for _ in 0..4 {
        dispatch_pending(&ctx, &mut store).unwrap();
    }
    host.reply("pool dispatch", 1, "no free slot on primary\n");
    for _ in 0..4 {
        dispatch_pending(&ctx, &mut store).unwrap();
    }
    host.reply("pool dispatch", 1, "unrefused t1 words\n");
    for _ in 0..4 {
        dispatch_pending(&ctx, &mut store).unwrap();
    }
    let job = &store.jobs["m1"];
    assert_eq!(job.status, JobStatus::Pending);
    assert_eq!(job.dispatch_refusals, 0);
    assert!(job.held.is_none());
    assert!(pool_dispatch_calls(&host) >= 8, "the retry never stops");
    assert!(!ctx.home.join("exceptions.txt").exists());
}

#[test]
fn a_correction_job_without_a_preset_task_names_its_step_task() {
    let (_dir, ctx, _host, mut store) =
        refusal_rig_for(None, Some("t-step"), 1, "refused t \"no usable bridge\"\n");
    for _ in 0..3 {
        dispatch_pending(&ctx, &mut store).unwrap();
    }
    assert!(store.jobs["m1"].held.is_some());
    let exceptions = std::fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert_eq!(exceptions.matches("job-dispatch-refused m1").count(), 1);
    assert!(
        exceptions.contains("`vivi pump requeue t-step`"),
        "{exceptions}"
    );
}

#[test]
fn a_held_job_without_a_step_names_the_job_release() {
    let (_dir, ctx, _host, mut store) =
        refusal_rig_for(Some("t1"), None, 1, "refused t1 \"no usable bridge\"\n");
    for _ in 0..3 {
        dispatch_pending(&ctx, &mut store).unwrap();
    }
    let exceptions = std::fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert!(
        exceptions.contains("`vivi pump requeue --job m1`"),
        "{exceptions}"
    );
}

#[test]
fn a_release_request_clears_the_hold_and_the_job_is_offered_again() {
    let (_dir, ctx, host, mut store) =
        refusal_rig_for(Some("t1"), None, 1, "refused t1 \"no usable bridge\"\n");
    for _ in 0..3 {
        dispatch_pending(&ctx, &mut store).unwrap();
    }
    assert!(store.jobs["m1"].held.is_some());
    let calls = pool_dispatch_calls(&host);

    let pump_db = PumpStore::open(&ctx.home.join(DB_FILENAME)).unwrap();
    pump_db.request_job_release("m1", 1).unwrap();
    pump_db.request_job_release("nope", 1).unwrap();
    host.reply(
        "pool dispatch",
        0,
        "dispatched t1 bridge slot-1 model/default unit/task\n",
    );
    assert_eq!(dispatch_pending(&ctx, &mut store).unwrap(), ["m1"]);

    let job = &store.jobs["m1"];
    assert_eq!(job.status, JobStatus::Dispatched);
    assert!(job.held.is_none());
    assert_eq!(job.dispatch_refusals, 0);
    assert_eq!(pool_dispatch_calls(&host), calls + 1);
    let exceptions = std::fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert!(
        exceptions.contains("resolved job-dispatch-refused m1 released by operator"),
        "{exceptions}"
    );
    assert!(
        host.logs()
            .iter()
            .any(|line| line.contains("job release ignored: nope is not held"))
    );
    assert!(pump_db.take_job_releases().unwrap().is_empty());
}

#[test]
fn a_config_reload_releases_held_jobs_and_resolves_their_alarm() {
    let (_dir, ctx, host, mut store) =
        refusal_rig_for(Some("t1"), None, 1, "refused t1 \"no usable bridge\"\n");
    for _ in 0..3 {
        dispatch_pending(&ctx, &mut store).unwrap();
    }
    assert!(store.jobs["m1"].held.is_some());

    release_held_after_reload(&ctx, &mut store);
    let job = &store.jobs["m1"];
    assert!(job.held.is_none());
    assert_eq!(job.dispatch_refusals, 0);
    assert_eq!(job.status, JobStatus::Pending);

    host.reply(
        "pool dispatch",
        0,
        "dispatched t1 bridge slot-1 model/default unit/task\n",
    );
    assert_eq!(dispatch_pending(&ctx, &mut store).unwrap(), ["m1"]);
    let log = std::fs::read_to_string(ctx.home.join("log.txt")).unwrap_or_default();
    let exceptions = std::fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert!(
        log.contains("job m1 released: the config changed")
            || exceptions.contains("released after a config reload"),
        "the release is recorded: {log}\n{exceptions}"
    );
}

/// The `--target` a job without one is dispatched with, for explicit targets
/// `ids` (none: the legacy single-repository fields).
fn untargeted_dispatch_target(ids: &[&str]) -> Option<String> {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = PumpConfig::default();
    for id in ids {
        cfg.targets.insert(
            (*id).to_owned(),
            PumpTarget {
                path: (*id).to_owned(),
                ..PumpTarget::default()
            },
        );
    }
    let (ctx, _host) = test_ctx(dir.path(), cfg);
    let command = command_for(&ctx, &PoolJob::from(spec("plan-1")), "studio", "t1");
    command
        .split(" --")
        .find_map(|part| part.strip_prefix("target "))
        .map(str::to_owned)
}

#[test]
fn a_job_without_a_target_works_on_the_projects_primary_target() {
    assert_eq!(untargeted_dispatch_target(&["app"]).as_deref(), Some("app"));
    assert_eq!(
        untargeted_dispatch_target(&["gradus", "default", "sandbox"]).as_deref(),
        Some("default")
    );
    assert_eq!(untargeted_dispatch_target(&["one", "two"]), None);
    assert_eq!(untargeted_dispatch_target(&[]), None);
}

#[test]
fn a_job_with_a_target_keeps_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = PumpConfig::default();
    cfg.targets
        .insert(String::from("app"), PumpTarget::default());
    let (ctx, _host) = test_ctx(dir.path(), cfg);
    let mut job = PoolJob::from(spec("rung-1"));
    job.target = Some(String::from("other"));
    let command = command_for(&ctx, &job, "studio", "t1");
    assert!(
        command.contains("--target other") && !command.contains("--target app"),
        "{command}"
    );
}
