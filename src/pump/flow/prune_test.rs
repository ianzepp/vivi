//! The prune phase: one cleanup job per ref a landed need owns.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use tempfile::{TempDir, tempdir};

use super::{PRUNE_FAILED, register};
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::need_branch::{LIVE, PRUNED};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobOutcome, JobSpec, JobStatus, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};
use crate::routing::{RoleRouting, RoutedRole};
use std::sync::Arc;

const NEED: &str = "n1";
const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

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

fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut registry = Registry::new();
    register(&mut registry);
    Ok(Rig {
        _dir: dir,
        ctx: with_merge_route(ctx),
        host,
        store,
        mail: FakeMail::new("mind"),
        jobs: Jobs::default(),
        registry,
    })
}

impl Rig {
    fn land(&self, kind: &str) -> Result<(), VivariumError> {
        self.store.insert_run(&RunRow {
            need: NEED.into(),
            run: 1,
            flow: kind.into(),
            flow_version: 1,
            kind: kind.into(),
            entry: "plan".into(),
            state: "done".into(),
        })?;
        for (node, task) in [("slice:A", "T1"), ("merge", "M1"), ("plan", "P1")] {
            self.store.insert_step(
                NEED,
                &StepRow {
                    run: 1,
                    node: node.into(),
                    task: task.into(),
                    state: StepState::Done,
                    verdict: None,
                },
            )?;
        }
        self.store.upsert_need_branch(&NeedBranchRow {
            need: NEED.into(),
            branch: format!("vivi/need/{NEED}"),
            base: Some(SHA.into()),
            status: LIVE.into(),
            run: 1,
            target: "default".into(),
            priors: vec![format!("vivi/need-prior/{NEED}-r1")],
        })
    }

    fn remote_has(&self, names: &[&str]) {
        let text: String = names
            .iter()
            .map(|name| format!("{SHA}\trefs/heads/{name}\n"))
            .collect();
        self.host.reply("ls-remote", 0, &text);
    }

    fn tick(&mut self) -> Result<(), VivariumError> {
        for phase in self.registry.phases() {
            phase.run(&self.ctx, &mut self.jobs, &self.store, &mut self.mail)?;
        }
        Ok(())
    }

    fn exceptions(&self) -> String {
        fs::read_to_string(self.ctx.home.join("exceptions.txt")).unwrap_or_default()
    }
}

#[test]
fn the_jobs_name_exactly_the_needs_refs() -> Result<(), VivariumError> {
    let mut rig = rig()?;
    rig.land("build")?;
    rig.remote_has(&[
        "vivi/need/n1",
        "vivi/need/n2",
        "vivi/need/n1x",
        "vivi/need-prior/n1-r1",
        "vivi/need-prior/n2-r1",
        "vivi/unit/T1-a1",
        "vivi/unit/T1-a2-s1",
        "vivi/unit/T10-a1",
        "vivi/unit/T2-a1",
        "vivi/unit/T1-a1/nested",
        "vivi/repair/n1-r1-a1",
        "pump/merge-main/default-merge-main-M1-0123456789ab",
        "pump/merge-main/default-merge-main-M2-0123456789ab",
    ]);
    rig.tick()?;
    let ids: BTreeSet<&str> = rig.jobs.jobs.keys().map(String::as_str).collect();
    let want: BTreeSet<&str> = [
        "prune-n1-vivi-need-n1",
        "prune-n1-vivi-need-prior-n1-r1",
        "prune-n1-vivi-unit-T1-a1",
        "prune-n1-vivi-unit-T1-a2-s1",
        "prune-n1-pump-merge-main-default-merge-main-M1-0123456789ab",
    ]
    .into();
    assert_eq!(ids, want);
    let job = &rig.jobs.jobs["prune-n1-vivi-unit-T1-a1"];
    assert_eq!(job.command.as_deref(), Some("true"));
    assert_eq!(job.role, "merge");
    assert_eq!(job.from_ref.as_deref(), Some(SHA));
    assert_eq!(
        job.delete_ref.as_deref(),
        Some("refs/heads/vivi/unit/T1-a1")
    );
    assert_eq!(job.delete_expect.as_deref(), Some(SHA));
    assert_eq!(job.target.as_deref(), Some("default"));
    assert_eq!(
        job.branch.as_deref(),
        Some("vivi/ref-delete/prune-n1-vivi-unit-T1-a1")
    );
    let row = rig.store.need_branch(NEED)?.expect("row");
    assert_eq!(row.status, PRUNED);
    Ok(())
}

#[test]
fn a_repair_run_also_prunes_its_repair_branches() -> Result<(), VivariumError> {
    let mut rig = rig()?;
    rig.land("repair")?;
    rig.remote_has(&["vivi/repair/n1-r1-a1", "vivi/repair/n2-r1-a1"]);
    rig.tick()?;
    let ids: Vec<&str> = rig.jobs.jobs.keys().map(String::as_str).collect();
    assert_eq!(ids, ["prune-n1-vivi-repair-n1-r1-a1"]);
    Ok(())
}

#[test]
fn a_restarted_repair_need_prunes_the_refs_of_every_run() -> Result<(), VivariumError> {
    let mut rig = rig()?;
    rig.land("repair")?;
    rig.store.set_run_state(NEED, 1, "superseded")?;
    rig.store.insert_run(&RunRow {
        need: NEED.into(),
        run: 2,
        flow: "repair".into(),
        flow_version: 1,
        kind: "repair".into(),
        entry: "fix1".into(),
        state: "done".into(),
    })?;
    rig.remote_has(&[
        "vivi/repair/n1-r1-a1",
        "vivi/repair/n1-r1-a2",
        "vivi/repair/n1-r2-a1",
        "vivi/repair/n2-r1-a1",
        "vivi/repair/n1-r2-a1/x",
    ]);
    rig.tick()?;
    let ids: BTreeSet<&str> = rig.jobs.jobs.keys().map(String::as_str).collect();
    let want: BTreeSet<&str> = [
        "prune-n1-vivi-repair-n1-r1-a1",
        "prune-n1-vivi-repair-n1-r1-a2",
        "prune-n1-vivi-repair-n1-r2-a1",
    ]
    .into();
    assert_eq!(ids, want);
    Ok(())
}

#[test]
fn a_second_tick_enqueues_nothing() -> Result<(), VivariumError> {
    let mut rig = rig()?;
    rig.land("build")?;
    rig.remote_has(&["vivi/need/n1", "vivi/unit/T1-a1"]);
    rig.tick()?;
    assert_eq!(rig.jobs.jobs.len(), 2);
    rig.jobs.jobs.clear();
    rig.tick()?;
    assert!(rig.jobs.jobs.is_empty());
    Ok(())
}

#[test]
fn a_live_run_or_a_missing_row_is_left_alone() -> Result<(), VivariumError> {
    let mut rig = rig()?;
    rig.land("build")?;
    rig.store.insert_run(&RunRow {
        need: NEED.into(),
        run: 2,
        flow: "build".into(),
        flow_version: 1,
        kind: "build".into(),
        entry: "plan".into(),
        state: "live".into(),
    })?;
    rig.remote_has(&["vivi/need/n1"]);
    rig.tick()?;
    assert!(rig.jobs.jobs.is_empty());
    assert_eq!(rig.store.need_branch(NEED)?.expect("row").status, LIVE);
    Ok(())
}

fn ended(id: &str, status: JobStatus, receipt: &str) -> PoolJob {
    let mut spec = JobSpec::new(id, "prune", "merge", "prune", "body");
    spec.delete_ref = Some(String::from("refs/heads/vivi/unit/T1-a1"));
    let mut job = PoolJob::from(spec);
    job.status = status;
    job.outcome = Some(JobOutcome {
        state: String::from("finished"),
        detail: String::new(),
        receipt: receipt.into(),
        at: 1,
    });
    job
}

#[test]
fn a_failed_cleanup_raises_the_alarm_once() -> Result<(), VivariumError> {
    let mut rig = rig()?;
    let deleted = format!("agent exit 0\nrunner delete: deleted refs/heads/x {SHA} {SHA} absent");
    let mismatch =
        format!("agent exit 0\nrunner delete: ref_mismatch refs/heads/x {SHA} {SHA} {SHA}");
    for job in [
        ended("prune-n1-lost", JobStatus::Lost, ""),
        ended("prune-n1-timeout", JobStatus::Timeout, ""),
        ended("prune-n1-bad", JobStatus::Finished, &mismatch),
        ended("prune-n1-ok", JobStatus::Finished, &deleted),
        ended("other-lost", JobStatus::Lost, ""),
    ] {
        rig.jobs.jobs.insert(job.id.clone(), job);
    }
    rig.tick()?;
    let text = rig.exceptions();
    assert_eq!(text.matches(PRUNE_FAILED).count(), 3, "{text}");
    assert!(text.contains("prune-n1-lost") && text.contains("prune-n1-timeout"));
    assert!(text.contains("prune-n1-bad") && !text.contains("prune-n1-ok"));
    assert!(!text.contains("other-lost"));
    for id in [
        "prune-n1-lost",
        "prune-n1-timeout",
        "prune-n1-bad",
        "prune-n1-ok",
    ] {
        assert!(rig.jobs.jobs[id].applied_at.is_some(), "{id}");
    }
    assert!(rig.jobs.jobs["other-lost"].applied_at.is_none());
    // Even if the job were seen again, the alarm does not repeat.
    for job in rig.jobs.jobs.values_mut() {
        job.applied_at = None;
    }
    rig.tick()?;
    assert_eq!(rig.exceptions().matches(PRUNE_FAILED).count(), 3);
    Ok(())
}
