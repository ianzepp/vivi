use std::fs;

use super::*;
use crate::pump::config::PumpConfig;
use crate::pump::host::test_ctx;
use crate::pump::jobs::{JobOutcome, JobSpec, JobStatus};
use crate::pump::state::State;

fn job(receipt: &str) -> PoolJob {
    let mut job = PoolJob::from(JobSpec::new(
        "planner-board-need1-100",
        "planner:board",
        "planner",
        "board planner need1",
        "plan",
    ));
    job.queued_at = 100;
    job.pool_candidates = vec!["here".into()];
    job.read_only = true;
    job.payload = serde_json::json!({
        "planner_output": { "out": "board/need1.out", "plan": "plans/need1.json" }
    });
    job.task = Some("task0001".into());
    job.pool = Some("here".into());
    job.dispatched_at = Some(100);
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: "finished".into(),
        detail: "agent_rc 0".into(),
        receipt: receipt.into(),
        at: 101,
    });
    job
}

#[test]
fn a_completed_plan_is_copied_and_its_receipt_is_kept_for_ingestion() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let receipt = "agent_rc 0 agent_s 1\nhand's final message:\nPUMP-PLAN-BEGIN\n{\"verdict\":\"none\"}\nPUMP-PLAN-END\n";
    let mut st = State::default();
    let job = job(receipt);
    let id = job.id.clone();
    st.jobs.insert(id.clone(), job);
    apply_completions(&ctx, &mut st);

    assert!(st.jobs[&id].applied_at.is_some());
    assert_eq!(
        fs::read_to_string(ctx.home.join("plans/need1.json")).unwrap(),
        "{\"verdict\":\"none\"}"
    );
    assert_eq!(
        fs::read_to_string(ctx.home.join("board/need1.out")).unwrap(),
        receipt
    );
    assert_eq!(
        fs::read_to_string(ctx.home.join("board/need1.out.rc")).unwrap(),
        "0\n"
    );
}

#[test]
fn a_plan_wrapped_in_a_code_fence_is_copied_bare() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let receipt = "agent_rc 0 agent_s 1\nhand's final message:\nPUMP-PLAN-BEGIN\n```json\n{\"verdict\":\"none\"}\n```\nPUMP-PLAN-END\n";
    let mut st = State::default();
    st.jobs
        .insert("planner-board-need1-100".into(), job(receipt));
    apply_completions(&ctx, &mut st);

    assert_eq!(
        fs::read_to_string(ctx.home.join("plans/need1.json")).unwrap(),
        "{\"verdict\":\"none\"}"
    );
}

#[test]
fn an_immediate_worker_exit_writes_the_failure_receipt_on_the_same_tick() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let receipt = "agent_rc 127 agent_s 0\nhand's final message:\nbash: pi: command not found\n";
    let mut st = State::default();
    let job = job(receipt);
    let id = job.id.clone();
    st.jobs.insert(id.clone(), job);
    apply_completions(&ctx, &mut st);

    assert!(st.jobs[&id].applied_at.is_some());
    assert!(!ctx.home.join("plans/need1.json").exists());
    assert_eq!(
        fs::read_to_string(ctx.home.join("board/need1.out.rc")).unwrap(),
        "127\n"
    );
    assert!(
        fs::read_to_string(ctx.home.join("board/need1.out"))
            .unwrap()
            .contains("bash: pi: command not found")
    );
}

fn planner_routing(roles: &[(&str, &str)]) -> crate::routing::RoleRouting {
    use crate::routing::{RoleRouting, RoutedPool, RoutedRole};
    RoleRouting {
        roles: roles
            .iter()
            .map(|(name, pool)| RoutedRole {
                name: (*name).into(),
                kind: Some("planner".into()),
                status: "active".into(),
                pool: Some((*pool).into()),
                read_only: None,
                model: None,
                models: Vec::new(),
            })
            .collect(),
        pools: roles
            .iter()
            .map(|(_, pool)| {
                (
                    (*pool).to_string(),
                    RoutedPool {
                        slots: 1,
                        provider: "local".into(),
                        fallback: None,
                        caps: std::collections::BTreeMap::new(),
                        models: Vec::new(),
                        has_bands: false,
                    },
                )
            })
            .collect(),
    }
}

fn enqueue_one(ctx: &Ctx, st: &mut State) -> Result<String, VivariumError> {
    enqueue(
        ctx,
        st,
        "board",
        "n1",
        100,
        "plan",
        60,
        "board/n1.out",
        "plans/n1.json",
        None,
    )
}

#[test]
fn planner_jobs_go_to_pump_planner_and_a_role_named_planner_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let ctx = ctx.with_routing(planner_routing(&[
        ("planner", "seat-pool"),
        ("pump-planner", "pump-pool"),
    ]));
    let mut st = State::default();
    let id = enqueue_one(&ctx, &mut st).unwrap();
    let job = &st.jobs()[&id];
    assert_eq!(job.role, "pump-planner");
    assert_eq!(job.pool_candidates, ["pump-pool"]);

    let ctx = ctx.with_routing(planner_routing(&[("planner", "seat-pool")]));
    let mut st = State::default();
    let error = enqueue_one(&ctx, &mut st).unwrap_err().to_string();
    assert!(error.contains("pump-planner"), "{error}");
    assert!(error.contains("vivi config migrate"), "{error}");
    assert!(st.jobs().is_empty());
}

/// A pool config the real `plan_job` accepts for a read-only planner run.
fn dispatchable_pool() -> crate::pool::config::PoolConfig {
    toml::from_str(
        r#"
repo = "o/r"
slots = 1
image = "debian:12"
build_dir = "target"
[fly]
app = "a"
[github]
app_id = 1
private_key = "k.pem"
[harness]
name = "fake"
run = "fake --model {model} @{brief}"
models = ["m/ok"]
[committer]
name = "bot"
email = "bot@example.com"
"#,
    )
    .unwrap()
}

/// The value after `--from` in a `pool dispatch` command line.
fn from_flag(command: &str) -> Option<String> {
    let mut words = command.split_whitespace();
    words.find(|word| *word == "--from")?;
    words.next().map(str::to_owned)
}

#[test]
fn a_planner_job_carries_a_start_ref_that_pool_dispatch_planning_accepts() {
    use crate::pool::dispatch::{DispatchFlags, plan_job};

    let dir = tempfile::tempdir().unwrap();
    let cfg = PumpConfig::default();
    let (ctx, host) = test_ctx(dir.path(), cfg);
    let ctx = ctx.with_routing(planner_routing(&[("pump-planner", "apool")]));
    host.reply(
        "pool status --porcelain",
        0,
        "slot apool bridge slot-1 free",
    );
    host.reply("task send", 0, "created 1234abcd");
    host.reply("pool dispatch", 0, "dispatched 1234abcd to apool");
    let mut st = State::default();
    let id = enqueue_one(&ctx, &mut st).unwrap();
    assert_eq!(st.jobs()[&id].from_ref.as_deref(), Some("main"));

    jobs::dispatch_pending(&ctx, &mut st).unwrap();
    let command = host
        .commands()
        .into_iter()
        .find(|cmd| cmd.contains("pool dispatch"))
        .expect("no pool dispatch command ran");
    let from = from_flag(&command).expect("the planner dispatch names no --from");

    // The same refusal `vivi pool dispatch` gives a job with no start ref.
    let flags = DispatchFlags {
        model: "m/ok".into(),
        from: Some(from.clone()),
        read_only: true,
        ..DispatchFlags::default()
    };
    let plan = plan_job(
        &dispatchable_pool(),
        "apool",
        (
            "1234abcd",
            "subject",
            &crate::letter::fixture::task_body("plan the need"),
        ),
        &flags,
        String::new(),
        String::from("job"),
    )
    .unwrap();
    assert_eq!(plan.refs.from, from);
}

fn three_targets() -> PumpConfig {
    use crate::pump::config::PumpTarget;
    let target = PumpTarget::default;
    PumpConfig {
        targets: std::collections::BTreeMap::from([
            ("radix".to_owned(), target()),
            ("gradus".to_owned(), target()),
            ("inferentia".to_owned(), target()),
        ]),
        ..PumpConfig::default()
    }
}

#[test]
fn a_planner_reads_every_configured_target_and_not_only_the_default_checkout() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), three_targets());
    let ctx = ctx.with_routing(planner_routing(&[("pump-planner", "apool")]));
    host.reply(
        "pool status --porcelain",
        0,
        "slot apool bridge slot-1 free\nslot apool bridge slot-2 free",
    );
    host.reply("task send", 0, "created 1234abcd");
    host.reply("pool dispatch", 0, "dispatched 1234abcd to apool");
    let mut st = State::default();

    // A board planner has no target of its own: it reads all three.
    let board = enqueue_one(&ctx, &mut st).unwrap();
    assert_eq!(
        st.jobs()[&board].extra_targets,
        ["gradus", "inferentia", "radix"]
    );
    assert!(st.jobs()[&board].read_only);

    // A split planner starts in its unit's target and reads the others.
    let split = enqueue(
        &ctx,
        &mut st,
        "split",
        "n1-1",
        100,
        "plan",
        60,
        "board/n1-1.out",
        "plans/n1-1.split.json",
        Some("gradus"),
    )
    .unwrap();
    assert_eq!(st.jobs()[&split].target.as_deref(), Some("gradus"));
    assert_eq!(st.jobs()[&split].extra_targets, ["inferentia", "radix"]);

    // The dispatch asks for each extra checkout, read-only.
    jobs::dispatch_pending(&ctx, &mut st).unwrap();
    let commands = host.commands();
    let board_cmd = commands
        .iter()
        .find(|cmd| cmd.contains("pool dispatch") && !cmd.contains("--target"))
        .expect("no board planner dispatch");
    for id in ["gradus", "inferentia", "radix"] {
        assert!(
            board_cmd.contains(&format!("--also-target {id}")),
            "{board_cmd}"
        );
    }
    assert!(board_cmd.contains("--read-only"), "{board_cmd}");
    let split_cmd = commands
        .iter()
        .find(|cmd| cmd.contains("--target gradus"))
        .expect("no split planner dispatch");
    assert!(!split_cmd.contains("--also-target gradus"), "{split_cmd}");
    assert!(split_cmd.contains("--also-target radix"), "{split_cmd}");
}

#[test]
fn a_single_repository_pump_gives_a_planner_no_extra_checkouts() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let ctx = ctx.with_routing(planner_routing(&[("pump-planner", "apool")]));
    let mut st = State::default();
    let id = enqueue_one(&ctx, &mut st).unwrap();
    assert!(st.jobs()[&id].extra_targets.is_empty());
}
