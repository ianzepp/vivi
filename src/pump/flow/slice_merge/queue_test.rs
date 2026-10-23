//! Tests for the one-slice-merge-per-need queue.

use std::collections::BTreeMap;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::{MERGE_QUEUED, run};
use crate::error::VivariumError;
use crate::pump::config::{PumpConfig, RungsConfig};
use crate::pump::flow::fake_mail::{Call, FakeMail};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::slice_merge::spec::KIND;
use crate::pump::flow::tick::OPERATOR_HOLD;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobOutcome, JobSpec, JobStatus, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};
use crate::routing::{RoleRouting, RoutedRole};

type R = Result<(), VivariumError>;

const MAIN: &str = "0123456789abcdef0123456789abcdef01234567";
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
    jobs: Jobs,
}

fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let mut cfg = PumpConfig::default();
    cfg.rungs = RungsConfig {
        task: Some(String::from("cargo test --lib")),
        timeout: Some(900),
        pools: vec![String::from("build-pool")],
        ..RungsConfig::default()
    };
    let (ctx, host) = test_ctx(dir.path(), cfg);
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
    host.reply(
        "ls-remote origin main",
        0,
        &format!("{MAIN}\trefs/heads/main\n"),
    );
    host.reply("ls-remote origin refs/heads/vivi/need/", 0, "");
    Ok(Rig {
        store: PumpStore::open(&dir.path().join(DB_FILENAME))?,
        _dir: dir,
        ctx: ctx.with_routing(routing),
        host,
        mail: FakeMail::new("mind"),
        jobs: Jobs::default(),
    })
}

impl Rig {
    fn run_row(&self, need: &str, state: &str) -> R {
        self.store.insert_run(&RunRow {
            need: need.into(),
            run: 1,
            flow: "build".into(),
            flow_version: 2,
            kind: "build".into(),
            entry: "plan".into(),
            state: state.into(),
        })
    }

    /// A built slice queued for merge, with its finished hand job.
    fn slice(&mut self, need: &str, id: &str, hand: bool) -> Result<StepRow, VivariumError> {
        let task = self.mail.send_step(
            "mind",
            "mind",
            &format!("slice {id}"),
            &format!("pump-step: {need} r1 slice:{id}\ntarget: default\n\ntitle\n\nbrief"),
            &[],
        )?;
        let row = StepRow {
            run: 1,
            node: format!("slice:{id}"),
            task: task.clone(),
            state: StepState::Waiting,
            verdict: Some(MERGE_QUEUED.into()),
        };
        self.store.insert_step(need, &row)?;
        if hand {
            let mut spec = JobSpec::new(format!("hand-{id}"), "hand", "hand", "hand", "body");
            spec.task = Some(task);
            spec.branch = Some(format!("vivi/unit/{id}-a1"));
            spec.payload = serde_json::json!({ "attempt": 1 });
            let mut job = PoolJob::from(spec);
            job.status = JobStatus::Finished;
            job.outcome = Some(JobOutcome {
                state: String::from("finished"),
                detail: String::new(),
                receipt: format!("agent exit 0\npush: pushed vivi/unit/{id}-a1 {HAND}\n"),
                at: 1,
            });
            self.jobs.jobs.insert(job.id.clone(), job);
        }
        Ok(row)
    }

    fn tick(&mut self) -> R {
        run(&self.ctx, &mut self.jobs, &self.store, &mut self.mail)
    }

    fn merges(&self, need: &str) -> Vec<&PoolJob> {
        self.jobs
            .jobs
            .values()
            .filter(|job| job.kind == KIND && job.payload["need"] == need)
            .collect()
    }

    fn state(&self, row: &StepRow) -> Result<Option<StepState>, VivariumError> {
        Ok(self.store.step_by_task(&row.task)?.map(|r| r.state))
    }
}

#[test]
fn two_queued_slices_of_one_need_start_one_merge() -> R {
    let mut rig = rig()?;
    rig.run_row("n1", "live")?;
    let first = rig.slice("n1", "A", true)?;
    let second = rig.slice("n1", "B", true)?;
    rig.tick()?;
    let merges = rig.merges("n1");
    assert_eq!(merges.len(), 1);
    assert_eq!(merges[0].task.as_deref(), Some(&*first.task));
    assert_eq!(rig.state(&first)?, Some(StepState::Running));
    assert_eq!(rig.state(&second)?, Some(StepState::Waiting));
    let queued = rig
        .store
        .step_by_task(&second.task)?
        .and_then(|r| r.verdict);
    assert_eq!(queued.as_deref(), Some(MERGE_QUEUED));
    Ok(())
}

#[test]
fn nothing_starts_while_the_first_merge_is_dispatched() -> R {
    let mut rig = rig()?;
    rig.run_row("n1", "live")?;
    rig.slice("n1", "A", true)?;
    rig.slice("n1", "B", true)?;
    rig.tick()?;
    for job in rig.jobs.jobs.values_mut().filter(|job| job.kind == KIND) {
        job.status = JobStatus::Dispatched;
    }
    rig.tick()?;
    assert_eq!(rig.merges("n1").len(), 1);
    Ok(())
}

#[test]
fn a_second_need_gets_its_own_merge_at_the_same_time() -> R {
    let mut rig = rig()?;
    rig.run_row("n1", "live")?;
    rig.run_row("n2", "live")?;
    rig.slice("n1", "A", true)?;
    rig.slice("n2", "C", true)?;
    rig.tick()?;
    assert_eq!(rig.merges("n1").len(), 1);
    assert_eq!(rig.merges("n2").len(), 1);
    Ok(())
}

#[test]
fn a_pending_merge_of_a_superseded_run_is_removed() -> R {
    let mut rig = rig()?;
    rig.run_row("n1", "live")?;
    let slice = rig.slice("n1", "A", true)?;
    rig.tick()?;
    assert_eq!(rig.merges("n1").len(), 1);
    rig.store.set_run_state("n1", 1, "superseded")?;
    rig.tick()?;
    assert!(rig.merges("n1").is_empty());
    assert!(
        rig.host
            .logs()
            .iter()
            .any(|line| line.contains("removed pending job hand-A-merge-t1")),
        "{:?}",
        rig.host.logs()
    );
    assert_eq!(rig.state(&slice)?, Some(StepState::Running));
    Ok(())
}

#[test]
fn a_queued_slice_with_no_hand_job_raises_once_and_enqueues_nothing() -> R {
    let mut rig = rig()?;
    rig.run_row("n1", "live")?;
    let slice = rig.slice("n1", "A", false)?;
    rig.tick()?;
    rig.tick()?;
    assert!(rig.merges("n1").is_empty());
    assert_eq!(rig.state(&slice)?, Some(StepState::Waiting));
    let needle = format!("EXCEPTION flow-slice-merge-failed {}", slice.task);
    let raised = rig
        .host
        .logs()
        .iter()
        .filter(|line| line.contains(&needle))
        .count();
    assert_eq!(raised, 1);
    Ok(())
}

#[test]
fn one_bad_slice_does_not_stop_another_need() -> R {
    let mut rig = rig()?;
    rig.run_row("n1", "live")?;
    rig.run_row("n2", "live")?;
    rig.slice("n1", "A", false)?;
    rig.slice("n2", "C", true)?;
    rig.tick()?;
    assert!(rig.merges("n1").is_empty());
    assert_eq!(rig.merges("n2").len(), 1);
    Ok(())
}

const NEED_TIP: &str = "4444444444444444444444444444444444444444";

#[test]
fn a_slice_already_on_the_need_branch_closes_merged_without_a_job() -> R {
    let mut rig = rig()?;
    rig.run_row("n1", "live")?;
    let slice = rig.slice("n1", "A", true)?;
    rig.host.reply(
        "ls-remote origin refs/heads/vivi/need/n1",
        0,
        &format!("{NEED_TIP}\trefs/heads/vivi/need/n1\n"),
    );
    rig.host.reply("cat-file -e", 0, "");
    rig.host.reply("merge-base --is-ancestor", 0, "");
    rig.tick()?;
    assert!(rig.merges("n1").is_empty());
    let row = rig.store.step_by_task(&slice.task)?.expect("row");
    assert_eq!(row.state, StepState::Done);
    assert_eq!(row.verdict.as_deref(), Some("merged"));
    assert!(rig.mail.calls.contains(&Call::CloseTask {
        handle: slice.task.clone(),
        verdict: String::from("merged"),
    }));
    Ok(())
}

#[test]
fn a_slice_absent_from_the_pump_repo_enqueues_one_job() -> R {
    let mut rig = rig()?;
    rig.run_row("n1", "live")?;
    rig.slice("n1", "A", true)?;
    rig.host.reply(
        "ls-remote origin refs/heads/vivi/need/n1",
        0,
        &format!("{NEED_TIP}\trefs/heads/vivi/need/n1\n"),
    );
    rig.host
        .reply("cat-file -e", 128, "fatal: Not a valid object name");
    rig.tick()?;
    assert_eq!(rig.merges("n1").len(), 1);
    Ok(())
}

#[test]
fn a_slice_present_but_not_an_ancestor_enqueues_one_job() -> R {
    let mut rig = rig()?;
    rig.run_row("n1", "live")?;
    rig.slice("n1", "A", true)?;
    rig.host.reply(
        "ls-remote origin refs/heads/vivi/need/n1",
        0,
        &format!("{NEED_TIP}\trefs/heads/vivi/need/n1\n"),
    );
    rig.host.reply("cat-file -e", 0, "");
    rig.host.reply("merge-base --is-ancestor", 1, "");
    rig.tick()?;
    assert_eq!(rig.merges("n1").len(), 1);
    Ok(())
}

#[test]
fn a_start_error_holds_the_slice_for_the_operator() -> R {
    let mut rig = rig()?;
    rig.run_row("n1", "live")?;
    let slice = rig.slice("n1", "A", false)?;
    rig.tick()?;
    let row = rig.store.step_by_task(&slice.task)?.expect("row");
    assert_eq!(row.state, StepState::Waiting);
    assert_eq!(row.verdict.as_deref(), Some(OPERATOR_HOLD));
    let raised = |rig: &Rig| {
        let needle = format!("EXCEPTION flow-slice-merge-failed {}", slice.task);
        rig.host
            .logs()
            .iter()
            .filter(|line| line.contains(&needle))
            .count()
    };
    assert_eq!(raised(&rig), 1);
    let commands = rig.host.commands().len();
    rig.tick()?;
    assert_eq!(raised(&rig), 1);
    assert_eq!(rig.host.commands().len(), commands);
    assert!(rig.merges("n1").is_empty());
    Ok(())
}

#[test]
fn a_slice_put_back_after_a_finished_merge_enqueues_the_next_try() -> R {
    let mut rig = rig()?;
    rig.run_row("n1", "live")?;
    let slice = rig.slice("n1", "A", true)?;
    rig.tick()?;
    assert!(rig.jobs.jobs.contains_key("hand-A-merge-t1"));
    if let Some(job) = rig.jobs.jobs.get_mut("hand-A-merge-t1") {
        job.status = JobStatus::Finished;
    }
    // The requeue puts the slice row back in the queue.
    rig.store
        .set_step_state(&slice.task, StepState::Waiting, Some(MERGE_QUEUED))?;
    rig.tick()?;
    assert!(rig.jobs.jobs.contains_key("hand-A-merge-t2"));
    assert_eq!(rig.merges("n1").len(), 2);
    assert_eq!(rig.state(&slice)?, Some(StepState::Running));
    Ok(())
}
