//! Tests for the ready-slices phase: dependency waits, a moving need branch
//! and idempotence.

use std::collections::BTreeMap;

use tempfile::{TempDir, tempdir};

use super::ReadySlices;
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::TickPhase;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::need_branch;
use crate::pump::flow::scope::ScopeState;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobStore, PoolJob};
use crate::pump::store::flow_links::SliceLink;
use crate::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

const NEED: &str = "n1";
const BASE: &str = "2222222222222222222222222222222222222222";

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
    _host: std::sync::Arc<FakeHost>,
    store: PumpStore,
    mail: FakeMail,
    jobs: Jobs,
}

fn branch_row(status: &str, run: u32) -> NeedBranchRow {
    NeedBranchRow {
        need: NEED.into(),
        branch: need_branch::branch(NEED),
        base: Some(BASE.into()),
        status: status.into(),
        run,
        target: "default".into(),
        priors: Vec::new(),
    }
}

fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let mut routing = (*ctx.routing).clone();
    routing.roles.push(RoutedRole {
        name: String::from("hand"),
        kind: Some(String::from("hand")),
        status: String::from("active"),
        pool: Some(String::from("test-planner-pool")),
        read_only: None,
        model: None,
        models: Vec::new(),
    });
    let ctx = ctx.with_routing(routing);
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    store.insert_run(&RunRow {
        need: NEED.into(),
        run: 1,
        flow: "build".into(),
        flow_version: 2,
        kind: "build".into(),
        entry: "plan".into(),
        state: "live".into(),
    })?;
    store.upsert_need_branch(&branch_row(need_branch::LIVE, 1))?;
    Ok(Rig {
        _dir: dir,
        ctx,
        _host: host,
        store,
        mail: FakeMail::new("pump"),
        jobs: Jobs::default(),
    })
}

impl Rig {
    /// A slice step in `state` with a scope and a link to `deps`.
    fn slice(
        &mut self,
        id: &str,
        state: StepState,
        deps: &[&str],
    ) -> Result<StepRow, VivariumError> {
        let task = self.mail.send_step(
            "pump",
            "pump",
            &format!("slice {id}"),
            &format!(
                "pump-step: {NEED} r1 slice:{id}\ntarget: default\n\ntitle {id}\n\nbrief {id}"
            ),
            &[],
        )?;
        let row = StepRow {
            run: 1,
            node: format!("slice:{id}"),
            task: task.clone(),
            state,
            verdict: None,
        };
        self.store.insert_step(NEED, &row)?;
        self.store
            .insert_slice_scope(&task, 1, &[format!("src/{id}.rs")])?;
        self.store.link_slice(&SliceLink {
            task,
            need: NEED.into(),
            run: 1,
            fanout: None,
            deps: deps.iter().map(|dep| (*dep).to_string()).collect(),
        })?;
        Ok(row)
    }

    fn tick(&mut self) -> R {
        ReadySlices.run(&self.ctx, &mut self.jobs, &self.store, &mut self.mail)
    }

    fn job_ids(&self) -> Vec<String> {
        self.jobs.jobs.keys().cloned().collect()
    }
}

#[test]
fn a_slice_with_no_deps_dispatches_one_job() -> R {
    let mut rig = rig()?;
    let slice = rig.slice("a", StepState::Pending, &[])?;

    rig.tick()?;

    assert_eq!(rig.job_ids(), vec![format!("slice-{}-a1", slice.task)]);
    assert_eq!(
        rig.store.step_by_task(&slice.task)?.unwrap().state,
        StepState::Running
    );
    Ok(())
}

#[test]
fn a_slice_waits_for_an_open_dep_and_dispatches_once_it_is_merged() -> R {
    let mut rig = rig()?;
    let dep = rig.slice("a", StepState::Running, &[])?;
    let slice = rig.slice("b", StepState::Pending, &[dep.task.as_str()])?;

    rig.tick()?;
    assert!(rig.job_ids().is_empty());

    rig.mail.close_task(&dep.task, "merged", None, None)?;
    rig.tick()?;
    assert_eq!(rig.job_ids(), vec![format!("slice-{}-a1", slice.task)]);
    Ok(())
}

#[test]
fn a_dep_closed_replaced_does_not_release_the_slice() -> R {
    let mut rig = rig()?;
    let dep = rig.slice("a", StepState::Running, &[])?;
    rig.slice("b", StepState::Pending, &[dep.task.as_str()])?;

    rig.mail.close_task(&dep.task, "replaced", None, None)?;
    rig.tick()?;

    assert!(rig.job_ids().is_empty());
    Ok(())
}

#[test]
fn a_no_defect_dep_releases_the_slice() -> R {
    let mut rig = rig()?;
    let dep = rig.slice("a", StepState::Running, &[])?;
    let slice = rig.slice("b", StepState::Pending, &[dep.task.as_str()])?;

    rig.mail.close_task(&dep.task, "no-defect", None, None)?;
    rig.tick()?;

    assert_eq!(rig.job_ids(), vec![format!("slice-{}-a1", slice.task)]);
    Ok(())
}

#[test]
fn a_scope_waiting_slice_is_not_dispatched() -> R {
    let mut rig = rig()?;
    let slice = rig.slice("a", StepState::Pending, &[])?;
    rig.store.set_scope_state(&slice.task, ScopeState::Wait)?;

    rig.tick()?;

    assert!(rig.job_ids().is_empty());
    Ok(())
}

#[test]
fn a_moving_need_branch_dispatches_nothing() -> R {
    for status in [need_branch::RENAMING, need_branch::DELETING] {
        let mut rig = rig()?;
        rig.store.upsert_need_branch(&branch_row(status, 1))?;
        rig.slice("a", StepState::Pending, &[])?;

        rig.tick()?;

        assert!(rig.job_ids().is_empty(), "{status}");
    }
    Ok(())
}

#[test]
fn a_live_branch_of_an_older_run_dispatches_nothing() -> R {
    let mut rig = rig()?;
    rig.store
        .upsert_need_branch(&branch_row(need_branch::LIVE, 0))?;
    rig.slice("a", StepState::Pending, &[])?;

    rig.tick()?;

    assert!(rig.job_ids().is_empty());
    Ok(())
}

#[test]
fn running_the_phase_twice_queues_one_job() -> R {
    let mut rig = rig()?;
    rig.slice("a", StepState::Pending, &[])?;

    rig.tick()?;
    rig.tick()?;

    assert_eq!(rig.job_ids().len(), 1);
    Ok(())
}
