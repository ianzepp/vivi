//! Tests for the slice merge hook and tick phase, through the tick.

use std::collections::BTreeMap;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::register;
use crate::error::VivariumError;
use crate::pump::config::{PumpConfig, RungsConfig};
use crate::pump::flow::advance::start::start;
use crate::pump::flow::collect_slice;
use crate::pump::flow::fake_mail::{Call, FakeMail};
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::slice_dispatch;
use crate::pump::flow::slice_merge::queue::MERGE_QUEUED;
use crate::pump::flow::slice_merge::spec::KIND;
use crate::pump::flow::tick::tick_with;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobOutcome, JobStatus, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

const NEED: &str = "n1";
const MAIN: &str = "0123456789abcdef0123456789abcdef01234567";
const S1: &str = "1111111111111111111111111111111111111111";
const HAND_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const HAND_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const BUILT: &str = "+++\nanswer = \"built\"\ndeviations = []\n+++\ndone";

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
    registry: Registry<Jobs, FakeMail>,
}

fn role(name: &str) -> RoutedRole {
    RoutedRole {
        name: name.to_owned(),
        kind: Some(name.to_owned()),
        status: String::from("active"),
        pool: Some(String::from("test-planner-pool")),
        read_only: None,
        model: None,
        models: Vec::new(),
    }
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
    let mut routing = (*ctx.routing).clone();
    routing.roles.push(role("hand"));
    routing.roles.push(role("merge"));
    host.reply(
        "ls-remote origin main",
        0,
        &format!("{MAIN}\trefs/heads/main\n"),
    );
    host.reply("ls-remote origin refs/heads/vivi/need/", 0, "");
    let mut mail = FakeMail::new("mind");
    MailWriter::open_record(&mut mail, NEED, "kind: build")?;
    let mut registry = Registry::<Jobs, FakeMail>::new();
    collect_slice::register_defaults(&mut registry);
    register(&mut registry);
    Ok(Rig {
        store: PumpStore::open(&dir.path().join(DB_FILENAME))?,
        _dir: dir,
        ctx: ctx.with_routing(routing),
        host,
        mail,
        jobs: Jobs::default(),
        registry,
    })
}

impl Rig {
    /// A dispatched slice whose hand job finished `built` with `sha`.
    fn built_slice(&mut self, id: &str, path: &str, sha: &str) -> Result<StepRow, VivariumError> {
        let task = self.mail.send_step(
            "mind",
            "mind",
            &format!("slice {id}"),
            &format!(
                "pump-step: {NEED} r1 slice:{id}\ntarget: default\n\ntitle {id}\n\nbrief {id}"
            ),
            &[],
        )?;
        let slice = StepRow {
            run: 1,
            node: format!("slice:{id}"),
            task: task.clone(),
            state: StepState::Pending,
            verdict: None,
        };
        self.store.insert_step(NEED, &slice)?;
        self.store
            .insert_slice_scope(&task, 1, &[path.to_owned()])?;
        let job = slice_dispatch::dispatch(
            &self.ctx,
            &mut self.jobs,
            &self.store,
            &self.mail,
            NEED,
            &slice,
            1,
        )?;
        let receipt = format!(
            "pool result j1: finished\npool p  slot s1  model m\nfrom main  default\n\
             push: pushed vivi/unit/x-a1 {sha}\nagent exit 0 in 12s, job 20s, started from {MAIN}\n\n\
             --- receipt\n{BUILT}\n\n--- stderr tail\n\n\n--- runner log tail\nlog line"
        );
        self.finish(&job, &receipt)?;
        Ok(slice)
    }

    fn finish(&mut self, id: &str, receipt: &str) -> R {
        let job = self
            .jobs
            .jobs
            .get_mut(id)
            .ok_or_else(|| VivariumError::Other(format!("no job {id}")))?;
        job.status = JobStatus::Finished;
        job.outcome = Some(JobOutcome {
            state: String::from("finished"),
            detail: String::new(),
            receipt: receipt.to_owned(),
            at: 1,
        });
        Ok(())
    }

    fn tick(&mut self) -> R {
        tick_with(
            &self.ctx,
            &mut self.jobs,
            &self.store,
            &mut self.mail,
            &self.registry,
        )
    }

    fn merges(&self) -> Vec<&PoolJob> {
        self.jobs
            .jobs
            .values()
            .filter(|job| job.kind == KIND)
            .collect()
    }

    fn row(&self, slice: &StepRow) -> Result<StepRow, VivariumError> {
        self.store
            .step_by_task(&slice.task)?
            .ok_or_else(|| VivariumError::Other(String::from("slice row is gone")))
    }

    fn merged_closes(&self) -> usize {
        self.mail
            .calls
            .iter()
            .filter(|call| matches!(call, Call::CloseTask { verdict, .. } if verdict == "merged"))
            .count()
    }
}

#[test]
fn green_alone_red_together_leaves_the_need_branch_at_the_first_merge() -> R {
    let mut rig = rig()?;
    start(&rig.ctx, &rig.store, &mut rig.mail, NEED, Some("plan"))?;
    let one = rig.built_slice("a", "src/a.rs", HAND_A)?;
    let two = rig.built_slice("b", "src/b.rs", HAND_B)?;

    rig.tick()?;
    let merges = rig.merges();
    assert_eq!(merges.len(), 1, "{:?}", rig.jobs.jobs.keys());
    let first_task = merges[0].task.clone().unwrap_or_default();
    let first_id = merges[0].id.clone();
    let (first, second) = if first_task == one.task {
        (&one, &two)
    } else {
        (&two, &one)
    };
    assert_eq!(rig.row(first)?.state, StepState::Running);
    let queued = rig.row(second)?;
    assert_eq!(queued.state, StepState::Waiting);
    assert_eq!(queued.verdict.as_deref(), Some(MERGE_QUEUED));

    rig.host.reply(
        "ls-remote origin refs/heads/vivi/need/",
        0,
        &format!("{S1}\trefs/heads/vivi/need/{NEED}\n"),
    );
    let published = format!(
        "agent exit 0 in 9s\nrunner publish: pushed refs/heads/vivi/need/{NEED} {HAND_A} {S1}"
    );
    rig.finish(&first_id, &published)?;
    rig.tick()?;
    assert_eq!(rig.merged_closes(), 1);
    let closed = rig.row(first)?;
    assert_eq!(closed.state, StepState::Done);
    assert_eq!(closed.verdict.as_deref(), Some("merged"));
    let started: Vec<&PoolJob> = rig
        .merges()
        .into_iter()
        .filter(|job| job.task.as_deref() == Some(&*second.task))
        .collect();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].from_ref.as_deref(), Some(S1));
    let second_id = started[0].id.clone();

    rig.finish(
        &second_id,
        "--- stderr tail\ntest result: FAILED. 1 failed\nagent exit 1 in 40s",
    )?;
    rig.tick()?;
    assert_eq!(
        rig.merged_closes(),
        1,
        "the red slice must not close merged"
    );
    assert_eq!(rig.row(second)?.state, StepState::Running);
    assert!(
        rig.jobs
            .jobs
            .contains_key(&format!("slice-{}-a2", second.task)),
        "{:?}",
        rig.jobs.jobs.keys()
    );
    assert_eq!(rig.merges().len(), 2, "no third merge job");
    Ok(())
}
