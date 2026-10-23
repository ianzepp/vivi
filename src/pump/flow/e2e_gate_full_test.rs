//! End to end gate: one build need from intake to done, over a real mailspace.
//!
//! Region: the whole build flow, `plan` -> `admit1` -> `slice` -> the slice
//! hands and their slice merges -> `build1` -> `audit1` and `test1` -> the
//! join `j1` (it keeps no step row) -> `merge` -> done. Edges covered: `planned`, `admitted`, `sliced`, `built`,
//! `verified`, `merged`, with the need and its pump record closed by the pump.

use super::e2e_rig::{Rig, agent_receipt, push_receipt, rig};
use crate::error::VivariumError;
use crate::pump::config::{PumpConfig, RungsConfig};
use crate::pump::flow::advance::start::SENTINEL_NODE;
use crate::pump::flow::fanout::is_slice_row;
use crate::pump::flow::index::StepState;
use crate::pump::flow::mail::{MailReader, NeedStatus};
use crate::pump::jobs::{JobStatus, PoolJob};
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

/// The main tip every ls-remote of main answers.
const MAIN: &str = "3333333333333333333333333333333333333333";
/// The need branch tip after the slice merges.
const TIP: &str = "2222222222222222222222222222222222222222";
/// The commit the final merge publishes on main.
const MERGED: &str = "4444444444444444444444444444444444444444";
/// The hand commits of the two slices.
const HAND_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const HAND_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

const PLANNED: &str = "+++\nanswer = \"planned\"\n+++\nthe plan: two slices";
const ADMITTED: &str = "+++\nanswer = \"admitted\"\n+++\nadmitted";
const SLICED: &str = "+++\nanswer = \"sliced\"\n\n\
     [[slice]]\nid = \"a\"\ntitle = \"first\"\nbrief = 'build a'\ntarget = \"default\"\n\
     write_scope = [\"src/a.rs\"]\ndeps = []\n\n\
     [[slice]]\nid = \"b\"\ntitle = \"second\"\nbrief = 'build b on a'\ntarget = \"default\"\n\
     write_scope = [\"src/b.rs\"]\ndeps = [\"a\"]\n+++\nsliced";
const BUILT: &str = "+++\nanswer = \"built\"\ndeviations = []\n+++\ndone";
const CLEAN: &str = "+++\nanswer = \"clean_pass\"\n+++\nclean";

fn config() -> PumpConfig {
    let mut cfg = PumpConfig::default();
    cfg.rungs = RungsConfig {
        task: Some(String::from("cargo test --lib")),
        need: Some(String::from("cargo test")),
        main: Some(String::from("cargo test --all")),
        timeout: Some(900),
        pools: vec![String::from("test-planner-pool")],
        ..RungsConfig::default()
    };
    cfg
}

fn role(name: &str, read_only: Option<bool>) -> RoutedRole {
    RoutedRole {
        name: name.into(),
        kind: Some(name.into()),
        status: String::from("active"),
        pool: Some(String::from("test-planner-pool")),
        read_only,
        model: None,
        models: Vec::new(),
    }
}

/// A build need at `plan`, with every role the flow needs routed and main at
/// [`MAIN`].
fn build() -> Result<Rig, VivariumError> {
    let mut rig = rig(config(), "+++\nkind = \"build\"\n+++\n", "plan")?;
    let mut routing = (*rig.ctx.routing).clone();
    routing.roles.push(role("admitter", Some(true)));
    routing.roles.push(role("slicer", Some(true)));
    routing.roles.push(role("hand", None));
    routing.roles.push(role("merge", Some(false)));
    rig.ctx = rig.ctx.with_routing(routing);
    rig.registry = crate::pump::flow::handlers::shipped(&rig.ctx);
    rig.git("main", MAIN);
    rig.project
        .host
        .reply("ls-remote origin refs/heads/vivi/need/", 0, "");
    // A remote pool: a hand commit is absent from the pump repo until it is
    // merged onto the need branch, so no slice is merged by ancestry.
    rig.project
        .host
        .reply("cat-file -e", 128, "fatal: Not a valid object name");
    let shown = serde_json::json!([
        {"handle": rig.need, "message_id": "m1", "from": "operator@local",
         "subject": "Need", "body": crate::letter::fixture::need_body(&["it works"], "build something in two slices")}
    ]);
    rig.project.host.reply("need show", 0, &shown.to_string());
    Ok(rig)
}

fn need_ref(rig: &Rig) -> String {
    format!("refs/heads/vivi/need/{}", rig.need)
}

/// Tick until `node` has an open job, then finish it with `receipt` and post
/// the same result on the step task, as the pool does; the next step's brief
/// reads it there.
fn finish(rig: &mut Rig, node: &str, receipt: &str) -> R {
    rig.tick_until(&format!("a {node} job"), |rig| rig.open_job(node).is_some())?;
    let id = rig
        .open_job(node)
        .map(|job| job.id)
        .ok_or_else(|| VivariumError::Message(format!("no open {node} job")))?;
    let row = rig
        .store
        .step_job(&id)?
        .ok_or_else(|| VivariumError::Message(format!("job {id} has no step row")))?;
    rig.finish(&id, JobStatus::Finished, receipt)?;
    rig.port.mailspace.record_pool_note(
        &row.task,
        "pool_collected",
        &format!("{{\"job\":\"{id}\"}}"),
        receipt,
    )
}

/// The open jobs of `kind` right now.
fn open_of_kind(rig: &Rig, kind: &str) -> Vec<PoolJob> {
    rig.jobs
        .jobs
        .values()
        .filter(|job| matches!(job.status, JobStatus::Pending | JobStatus::Dispatched))
        .filter(|job| job.kind == kind)
        .cloned()
        .collect()
}

/// Tick until an open job of `kind` exists, then return it.
fn wait_for(rig: &mut Rig, kind: &str) -> Result<PoolJob, VivariumError> {
    rig.tick_until(&format!("an open {kind} job"), |rig| {
        !open_of_kind(rig, kind).is_empty()
    })?;
    open_of_kind(rig, kind)
        .pop()
        .ok_or_else(|| VivariumError::Message(format!("no open {kind} job")))
}

/// Drive one slice: its hand job publishes `sha`, then its model-free slice
/// merge publishes the need branch. Returns the slice task.
fn build_slice(rig: &mut Rig, sha: &str) -> Result<String, VivariumError> {
    let hand = wait_for(rig, "hand")?;
    let task = hand
        .task
        .clone()
        .ok_or_else(|| VivariumError::Message("hand job has no task".into()))?;
    let pushed = format!("pushed vivi/unit/x-a1 {sha}");
    rig.finish(
        &hand.id,
        JobStatus::Finished,
        &push_receipt(0, &pushed, "", BUILT),
    )?;
    let merge = wait_for(rig, "slice-merge")?;
    assert_eq!(merge.task.as_deref(), Some(task.as_str()));
    assert!(merge.command.is_some(), "the slice merge is model-free");
    let publish = format!("runner publish: pushed {} {TIP} {TIP}", need_ref(rig));
    rig.finish(
        &merge.id,
        JobStatus::Finished,
        &push_receipt(0, "", &publish, ""),
    )?;
    // The slice closes `merged` on a following tick.
    rig.tick_until("the slice closed merged", |rig| {
        rig.store.step_by_task(&task).is_ok_and(|row| {
            row.is_some_and(|row| {
                row.state == StepState::Done && row.verdict.as_deref() == Some("merged")
            })
        })
    })?;
    Ok(task)
}

fn run_done(rig: &Rig) -> bool {
    rig.runs()
        .is_ok_and(|runs| runs.iter().any(|run| run.state == "done"))
}

#[test]
fn one_build_need_runs_from_intake_to_done() -> R {
    let mut rig = build()?;

    // plan -> admit1 -> slice, each a pool job finished with its envelope.
    finish(&mut rig, "plan", &agent_receipt(0, PLANNED))?;
    finish(&mut rig, "admit1", &agent_receipt(0, ADMITTED))?;
    finish(&mut rig, "slice", &agent_receipt(0, SLICED))?;

    // build1 fans out two slices; the second waits on the first. Each slice
    // is a hand job, then a model-free slice merge onto the need branch.
    let first = build_slice(&mut rig, HAND_A)?;
    rig.git(&need_ref(&rig), TIP);
    let second = build_slice(&mut rig, HAND_B)?;
    assert_ne!(first, second);

    // Both slices merged: build1 answers built; audit1 and test1 run.
    rig.tick_until("the audit1 and test1 jobs", |rig| {
        rig.open_job("audit1").is_some() && rig.open_job("test1").is_some()
    })?;
    finish(&mut rig, "audit1", &agent_receipt(0, CLEAN))?;
    finish(&mut rig, "test1", &agent_receipt(0, "all tests passed"))?;

    // The join verifies, and the merge publishes main.
    let merged = push_receipt(
        0,
        "",
        &format!("runner publish: pushed refs/heads/main {MERGED} {MERGED}"),
        "",
    );
    finish(&mut rig, "merge", &merged)?;
    rig.tick_until("run done", run_done)?;

    let rows: Vec<String> = rig
        .store
        .steps_of(&rig.need, 1)?
        .into_iter()
        .filter(|row| row.node != SENTINEL_NODE && !is_slice_row(row))
        .map(|row| row.node)
        .collect();
    assert_eq!(
        rows,
        [
            "plan", "admit1", "slice", "build1", "audit1", "test1", "merge"
        ]
    );
    let runs = rig.runs()?;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].state, "done", "{}", rig.exceptions());
    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Done);
    assert_eq!(
        rig.port.last_closed_record(&rig.need)?.as_deref(),
        Some("landed")
    );
    assert_eq!(rig.exceptions(), "");
    Ok(())
}
