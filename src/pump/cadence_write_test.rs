use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;

use tempfile::tempdir;

use super::{prepare_due, state};
use crate::mailspace::Mailspace;
use crate::pump::config::PumpConfig;
use crate::pump::gate;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{self, JobStatus, dispatch_pending};
use crate::pump::merge::{apply_dispatched, dispatch_pool};
use crate::pump::state::{State, Status, Unit};
use crate::routing;

const CADENCE_TASK: &str = "c0000001";
const MERGE_TASK: &str = "c0000002";
const RED_GATE_TASK: &str = "c0000003";
const GREEN_GATE_TASK: &str = "c0000004";
const CANDIDATE: &str = "0123456789abcdef0123456789abcdef01234567";
const INTEGRATION_TIP: &str = "abcdefabcdefabcdefabcdefabcdefabcdefabcd";
const DESCENDANT_TIP: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn setup() -> (tempfile::TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".vivi")).unwrap();
    fs::write(
        dir.path().join(".vivi/mailspace.toml"),
        concat!(
            "name = \"cadence-write-test\"\n\n",
            "[[identities]]\nname = \"mind\"\nkind = \"mind\"\n\n",
            "[[identities]]\nname = \"hand\"\nkind = \"hand\"\npool = \"local\"\n\n",
            "[[identities]]\nname = \"cadence-writer\"\nkind = \"hand\"\npool = \"local\"\ncadence = \"1h\"\nread_only = false\n\n",
            "[[identities]]\nname = \"merge\"\nkind = \"merge\"\npool = \"local\"\n\n",
            "[[identities]]\nname = \"gate\"\nkind = \"gate\"\npool = \"local\"\nread_only = true\n\n",
            "[pools.local]\nprovider = \"local\"\nslots = 1\n\n",
            "[pump]\nfrom = \"mind\"\nto = \"hand\"\nreply_from = \"mind\"\nbranch = \"next\"\n\n",
            "[pump.gate]\ncommand = \"true\"\npools = [\"local\"]\n"
        ),
    )
    .unwrap();
    let mut mailspace = Mailspace::discover(Some(dir.path())).unwrap();
    mailspace
        .set_charter("cadence-writer", "Write a useful periodic update.\n")
        .unwrap();
    let cfg = mailspace.config.pump.clone().unwrap_or_default();
    let (mut ctx, host) = test_ctx(dir.path(), cfg);
    ctx.project = dir.path().to_path_buf();
    ctx.routing = Arc::new(routing::from_mailspace(&mailspace.config));
    ctx.mailspace = mailspace.mailspace_id().to_string();
    ctx.pool_sources = Arc::new(ctx.cfg.pool_sources(&mailspace.config, dir.path()));
    host.set_now(4_000_000_000);
    (dir, ctx, host)
}

fn dispatch_role_job(ctx: &Ctx, st: &mut State, host: &FakeHost, role: &str, task: &str) {
    host.reply("pool status", 0, "slot local bridge slot-1 free\n");
    host.reply(
        &format!("--to {role}"),
        0,
        &format!("created hand {task}\n"),
    );
    host.reply("pool dispatch", 0, "dispatched task local slot-1\n");
    assert_eq!(dispatch_pending(ctx, st).unwrap().len(), 1);
    assert!(st.jobs.values().any(|job| {
        job.role == role && job.task.as_deref() == Some(task) && job.status == JobStatus::Dispatched
    }));
}

fn finish_job(ctx: &Ctx, st: &mut State, host: &FakeHost, task: &str, detail: &str, receipt: &str) {
    host.reply(&format!("task show {task} --latest"), 0, receipt);
    let seen = BTreeMap::from([(
        task.to_owned(),
        (String::from("finished"), detail.to_owned()),
    )]);
    jobs::apply_reconcile(ctx, st, &seen);
}

fn cadence_job_count(st: &State) -> usize {
    st.jobs.values().filter(|job| job.kind == "cadence").count()
}

fn run_cadence(ctx: &Ctx, st: &mut State, host: &FakeHost) -> String {
    prepare_due(ctx, st);
    let cadence_id = state(&st).runs["cadence-writer"].job.clone();
    let cadence = &st.jobs[&cadence_id];
    assert!(cadence.integration_required);
    assert!(!cadence.read_only);
    let branch = cadence.branch.clone().expect("write cadence branch");
    assert!(branch.starts_with("cadence/cadence-writer-"));

    dispatch_role_job(ctx, st, host, "cadence-writer", CADENCE_TASK);
    finish_job(
        ctx,
        st,
        host,
        CADENCE_TASK,
        "pushed cadence branch",
        "push: pushed cadence branch\nagent exit 0 in 2s\n",
    );
    prepare_due(ctx, st);
    let cadence_state = state(st);
    let run = &cadence_state.runs["cadence-writer"];
    assert_eq!(run.outcome.as_deref(), Some("awaiting-merge-gate"));
    assert_eq!(run.ended_at, None);
    assert_eq!(cadence_job_count(st), 1, "an unmerged run stays active");
    cadence_id
}

fn merge_cadence_branch(ctx: &Ctx, st: &mut State, host: &FakeHost, cadence_id: &str) {
    let merge_ids = dispatch_pool(ctx, st).unwrap();
    assert_eq!(merge_ids.len(), 1);
    let merge_id = merge_ids[0].clone();
    assert_eq!(st.jobs[&merge_id].source_jobs, vec![cadence_id.to_owned()]);
    dispatch_role_job(ctx, st, host, "merge", MERGE_TASK);
    let merge_result = serde_json::json!({
        "version": 1,
        "target": "default",
        "tip": CANDIDATE,
        "sources": [{"kind": "job", "id": cadence_id, "status": "merged"}]
    });
    host.reply("fetch --quiet origin refs/heads/next", 0, "");
    host.reply(
        "rev-parse --verify --quiet FETCH_HEAD^{commit}",
        0,
        &format!("{INTEGRATION_TIP}\n"),
    );
    host.reply(
        &format!("merge-base --is-ancestor {CANDIDATE} {INTEGRATION_TIP}"),
        0,
        "",
    );
    finish_job(
        ctx,
        st,
        host,
        MERGE_TASK,
        "integrated cadence branch",
        &format!(
            "runner publish: pushed refs/heads/next {CANDIDATE} {INTEGRATION_TIP}\n\
             agent exit 0 in 3s\nPUMP_MERGE_RESULT={merge_result}\n"
        ),
    );
    apply_dispatched(ctx, st);
    assert_eq!(
        st.jobs[cadence_id].merged_tip.as_deref(),
        Some(INTEGRATION_TIP)
    );
    assert!(jobs::sources_waiting_for_gate(st, "default").contains(&cadence_id.to_owned()));
    prepare_due(ctx, st);
    assert_eq!(
        cadence_job_count(st),
        1,
        "a merged but ungated run stays active"
    );
}

fn insert_delivered_unit(st: &mut State) {
    st.order.push(String::from("unit-1"));
    st.units.insert(
        String::from("unit-1"),
        Unit {
            need: String::new(),
            status: Status::Delivered,
            branch: Some(String::from("unit/unit-1-a1")),
            ..Unit::default()
        },
    );
}

fn red_gate_does_not_repeat(ctx: &Ctx, st: &mut State, host: &FakeHost, cadence_id: &str) {
    host.reply(
        "git ls-remote",
        0,
        &format!("{INTEGRATION_TIP}\trefs/heads/next\n"),
    );
    gate::start_gate(ctx, st);
    let gate_id = st.last_gate.job_id.clone().expect("due red gate");
    assert_eq!(st.jobs[&gate_id].source_jobs, vec![cadence_id.to_owned()]);
    dispatch_role_job(ctx, st, host, "gate", RED_GATE_TASK);
    finish_job(
        ctx,
        st,
        host,
        RED_GATE_TASK,
        "new failure",
        "agent exit 0 in 1s\nFAILING-BEGIN\ncrate::new::failure\nFAILING-END\n",
    );
    host.reply("pool status", 0, "slot local bridge slot-1 occupied\n");
    gate::collect_completed(ctx, st);
    assert_eq!(
        st.jobs[cadence_id].gate_attempted_tip.as_deref(),
        Some(INTEGRATION_TIP)
    );
    assert_eq!(st.jobs[cadence_id].gated_tip, None);
    gate::start_gate(ctx, st);
    assert!(
        st.last_gate.job_id.is_none(),
        "the same red tip is not retried"
    );
    assert_eq!(st.jobs.values().filter(|job| job.kind == "gate").count(), 1);
}

fn accept_clean_gate(ctx: &Ctx, st: &mut State, host: &FakeHost, cadence_id: &str) {
    host.reply(
        "git ls-remote",
        0,
        &format!("{DESCENDANT_TIP}\trefs/heads/next\n"),
    );
    host.reply(
        &format!("merge-base --is-ancestor {INTEGRATION_TIP} {DESCENDANT_TIP}"),
        0,
        "",
    );
    gate::start_gate(ctx, st);
    let gate_id = st.last_gate.job_id.clone().expect("due descendant gate");
    assert_eq!(st.jobs[&gate_id].source_jobs, vec![cadence_id.to_owned()]);
    gate::start_gate(ctx, st);
    assert_eq!(
        st.jobs.values().filter(|job| job.kind == "gate").count(),
        2,
        "an active gate is not duplicated"
    );
    dispatch_role_job(ctx, st, host, "gate", GREEN_GATE_TASK);
    finish_job(
        ctx,
        st,
        host,
        GREEN_GATE_TASK,
        "gate passed",
        "agent exit 0 in 1s\nFAILING-BEGIN\n\nFAILING-END\n",
    );
    host.reply(
        "ls-remote origin refs/heads/next",
        1,
        "remote unavailable\n",
    );
    gate::collect_completed(ctx, st);
    assert_eq!(st.jobs[cadence_id].gated_tip, None);
    assert!(st.gates.iter().any(|gate| {
        gate.tip == DESCENDANT_TIP && gate.names.as_ref().is_some_and(Vec::is_empty)
    }));
    host.reply(
        "ls-remote origin refs/heads/next",
        0,
        &format!("{DESCENDANT_TIP}\trefs/heads/next\n"),
    );
    gate::collect_completed(ctx, st);
    let ancestry = format!("merge-base --is-ancestor {INTEGRATION_TIP} {DESCENDANT_TIP}");
    assert!(
        host.commands()
            .iter()
            .any(|command| command.contains(&ancestry)),
        "the descendant gate is accepted only after the ancestry check"
    );
    assert!(st.last_gate.job_id.is_none());
    gate::hold_cycle(ctx, st);
    gate::start_gate(ctx, st);
    assert_eq!(
        st.jobs.values().filter(|job| job.kind == "gate").count(),
        2,
        "the same accepted role source does not start another gate"
    );
    prepare_due(ctx, st);
    let cadence_state = state(st);
    let run = &cadence_state.runs["cadence-writer"];
    assert_eq!(run.outcome.as_deref(), Some("success"));
    assert!(run.ended_at.is_some());
    assert_eq!(
        st.jobs[cadence_id].gated_tip.as_deref(),
        Some(DESCENDANT_TIP)
    );
    assert_eq!(
        cadence_job_count(st),
        1,
        "the next hourly run is not due yet"
    );
}

#[test]
fn writable_cadence_branch_merges_waits_for_gate_and_blocks_the_next_run() {
    let (_dir, ctx, host) = setup();
    let mut st = State {
        baseline: Some(Vec::new()),
        ..State::default()
    };
    let cadence_id = run_cadence(&ctx, &mut st, &host);
    merge_cadence_branch(&ctx, &mut st, &host, &cadence_id);
    insert_delivered_unit(&mut st);
    assert!(dispatch_pool(&ctx, &mut st).unwrap().is_empty());
    assert_eq!(st.units["unit-1"].status, Status::Delivered);
    red_gate_does_not_repeat(&ctx, &mut st, &host, &cadence_id);
    assert!(dispatch_pool(&ctx, &mut st).unwrap().is_empty());
    assert_eq!(st.units["unit-1"].status, Status::Delivered);
    accept_clean_gate(&ctx, &mut st, &host, &cadence_id);
    let next_merge = dispatch_pool(&ctx, &mut st).unwrap();
    assert_eq!(next_merge.len(), 1);
    assert_eq!(st.units["unit-1"].status, Status::Merging);
}
