//! Tests for the slice merge job spec.

use std::collections::BTreeMap;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::{KIND, hand_commit, hand_job_of, spec};
use crate::error::VivariumError;
use crate::pump::config::{PumpConfig, RungsConfig};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::mail::MailWriter;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobOutcome, JobSpec, JobStatus, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};
use crate::routing::{RoleRouting, RoutedRole};

type R = Result<(), VivariumError>;

const NEED: &str = "n1";
const MAIN: &str = "0123456789abcdef0123456789abcdef01234567";
const TIP: &str = "1111111111111111111111111111111111111111";
const HAND: &str = "2222222222222222222222222222222222222222";

#[derive(Default)]
struct Jobs {
    jobs: BTreeMap<String, PoolJob>,
}

impl JobStore for Jobs {
    fn jobs(&self) -> &BTreeMap<String, PoolJob> {
        &self.jobs
    }

    fn jobs_mut(&mut self) -> &mut BTreeMap<String, PoolJob> {
        &mut self.jobs
    }
}

struct Rig {
    _dir: TempDir,
    ctx: Ctx,
    host: Arc<FakeHost>,
    store: PumpStore,
    mail: FakeMail,
    run: RunRow,
    slice: StepRow,
}

fn config(task_rung: bool) -> PumpConfig {
    let mut cfg = PumpConfig::default();
    cfg.merge.format = String::from("cargo fmt --all");
    cfg.merge.remote_limit = 100;
    cfg.rungs = RungsConfig {
        task: task_rung.then(|| String::from("cargo test --lib")),
        timeout: Some(900),
        pools: vec![String::from("build-pool")],
        ..RungsConfig::default()
    };
    cfg
}

fn with_merge_route(ctx: Ctx) -> Ctx {
    let mut routing: RoleRouting = (*ctx.routing).clone();
    routing.roles.push(RoutedRole {
        name: String::from("merge"),
        kind: Some(String::from("merge")),
        status: String::from("active"),
        pool: Some(String::from("test-planner-pool")),
        read_only: Some(false),
        model: None,
        models: Vec::new(),
    });
    ctx.with_routing(routing)
}

fn rig(cfg: PumpConfig) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, host) = test_ctx(dir.path(), cfg);
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("mind");
    let task = mail.send_step(
        "mind",
        "mind",
        "slice A",
        &format!("pump-step: {NEED} r1 slice:A\ntarget: default\n\ntitle\n\nbrief"),
        &[],
    )?;
    let run = RunRow {
        need: NEED.into(),
        run: 1,
        flow: "build".into(),
        flow_version: 1,
        kind: "build".into(),
        entry: "plan".into(),
        state: "live".into(),
    };
    store.insert_run(&run)?;
    let slice = StepRow {
        run: 1,
        node: String::from("slice:A"),
        task,
        state: StepState::Waiting,
        verdict: None,
    };
    store.insert_step(NEED, &slice)?;
    host.reply(
        "ls-remote origin main",
        0,
        &format!("{MAIN}\trefs/heads/main\n"),
    );
    Ok(Rig {
        _dir: dir,
        ctx: with_merge_route(ctx),
        host,
        store,
        mail,
        run,
        slice,
    })
}

fn publish_receipt(sha: &str) -> String {
    format!(
        "agent exit 0\nrunner publish: pushed refs/heads/vivi/unit/T1-a1 {sha} {sha}\n\nhand's final message:\nok"
    )
}

fn hand_job(id: &str, task: &str, attempt: u32, queued_at: u64, receipt: &str) -> PoolJob {
    let mut spec = JobSpec::new(id, "hand", "hand", "hand", "body");
    spec.task = Some(task.to_owned());
    spec.branch = Some(format!("vivi/unit/{task}-a{attempt}"));
    spec.payload = serde_json::json!({ "attempt": attempt });
    let mut job = PoolJob::from(spec);
    job.status = JobStatus::Finished;
    job.queued_at = queued_at;
    job.outcome = Some(JobOutcome {
        state: String::from("finished"),
        detail: String::new(),
        receipt: receipt.to_owned(),
        at: 1,
    });
    job
}

impl Rig {
    fn hand(&self) -> PoolJob {
        hand_job("hand-1", &self.slice.task, 1, 10, &publish_receipt(HAND))
    }

    fn spec(&self) -> Result<JobSpec, VivariumError> {
        spec(
            &self.ctx,
            &self.store,
            &self.mail,
            &self.run,
            &self.slice,
            &self.hand(),
        )
    }

    fn no_need_branch(&self) {
        self.host
            .reply("ls-remote origin refs/heads/vivi/need/", 0, "");
    }
}

#[test]
fn from_ref_is_the_need_branch_tip_when_the_branch_exists() -> R {
    let rig = rig(config(true))?;
    rig.host.reply(
        "ls-remote origin refs/heads/vivi/need/",
        0,
        &format!("{TIP}\trefs/heads/vivi/need/{NEED}\n"),
    );
    let job = rig.spec()?;
    assert_eq!(job.from_ref.as_deref(), Some(TIP));
    assert_eq!(job.payload["before"], serde_json::json!(TIP));
    Ok(())
}

#[test]
fn from_ref_is_the_base_when_the_branch_does_not_exist() -> R {
    let rig = rig(config(true))?;
    rig.no_need_branch();
    let job = rig.spec()?;
    assert_eq!(job.from_ref.as_deref(), Some(MAIN));
    assert!(job.payload["before"].is_null());
    Ok(())
}

#[test]
fn the_command_merges_the_hand_commit_then_formats_then_runs_the_task_rung() -> R {
    let rig = rig(config(true))?;
    rig.no_need_branch();
    let job = rig.spec()?;
    assert_eq!(
        job.command.as_deref(),
        Some(&*format!(
            "git merge --no-ff --no-edit -m \"Merge one slice onto the need branch\" {HAND} && cargo fmt --all && cargo test --lib"
        ))
    );
    assert_eq!(job.with, vec![HAND.to_owned()]);
    Ok(())
}

#[test]
fn an_empty_formatter_is_skipped() -> R {
    let mut cfg = config(true);
    cfg.merge.format = String::new();
    let rig = rig(cfg)?;
    rig.no_need_branch();
    let command = rig.spec()?.command.unwrap_or_default();
    assert!(
        command.ends_with(&format!("{HAND} && cargo test --lib")),
        "{command}"
    );
    Ok(())
}

#[test]
fn the_job_publishes_the_need_branch_under_the_merge_role() -> R {
    let rig = rig(config(true))?;
    rig.no_need_branch();
    let job = rig.spec()?;
    assert_eq!(job.kind, KIND);
    assert_eq!(job.role, "merge");
    assert_eq!(job.id, "hand-1-merge");
    assert_eq!(
        job.branch.as_deref(),
        Some(&*format!("vivi/unit/{}-a1-merge", rig.slice.task))
    );
    assert_eq!(job.publish_ref.as_deref(), Some("refs/heads/vivi/need/n1"));
    assert_eq!(job.publish_contains.as_deref(), Some(HAND));
    assert_eq!(job.pool_candidates, vec![String::from("build-pool")]);
    assert_eq!(job.budget, 1000);
    assert_eq!(job.target.as_deref(), Some("default"));
    assert_eq!(job.task.as_deref(), Some(&*rig.slice.task));
    assert_eq!(job.payload["class"], "slice-merge");
    assert_eq!(job.payload["hand_job"], "hand-1");
    assert_eq!(job.payload["hand_sha"], HAND);
    assert_eq!(job.payload["attempt"], 1);
    assert_eq!(job.payload["node"], "slice:A");
    assert_eq!(job.payload["kind"], "build");
    Ok(())
}

#[test]
fn a_missing_task_rung_is_an_error() -> R {
    let rig = rig(config(false))?;
    rig.no_need_branch();
    let err = rig.spec().expect_err("no task rung");
    assert!(
        err.to_string().contains("rung task is not configured"),
        "{err}"
    );
    Ok(())
}

#[test]
fn hand_job_of_picks_the_newest_attempt_of_the_task() {
    let mut jobs = Jobs::default();
    for job in [
        hand_job("h1", "T1", 1, 50, ""),
        hand_job("h2", "T1", 2, 10, ""),
        hand_job("h3", "T2", 3, 90, ""),
    ] {
        jobs.jobs.insert(job.id.clone(), job);
    }
    let mut running = hand_job("h4", "T1", 4, 99, "");
    running.status = JobStatus::Dispatched;
    jobs.jobs.insert(running.id.clone(), running);
    assert_eq!(
        hand_job_of(&jobs, "T1").map(|job| job.id).as_deref(),
        Some("h2")
    );
    assert!(hand_job_of(&jobs, "T9").is_none());
}

#[test]
fn hand_commit_reads_both_receipt_forms() {
    let runner = hand_job("h", "T1", 1, 1, &publish_receipt(HAND));
    assert_eq!(hand_commit(&runner).as_deref(), Some(HAND));
    let summary = hand_job(
        "h",
        "T1",
        1,
        1,
        &format!("agent exit 0\npush: pushed vivi/unit/T1-a1 {TIP}\n\n--- receipt\nmore"),
    );
    assert_eq!(hand_commit(&summary).as_deref(), Some(TIP));
    let none = hand_job("h", "T1", 1, 1, "agent exit 0\npush: no_changes\n");
    assert_eq!(hand_commit(&none), None);
}
