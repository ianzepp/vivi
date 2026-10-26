//! The dispatcher honors a durable retry deadline without occupying capacity.

use std::collections::BTreeMap;

use super::{JobStatus, dispatch_pending};
use crate::pump::config::PumpConfig;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::replay;
use crate::pump::jobs::{JobSpec, JobStore, PoolJob};
use crate::pump::state::State;
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

/// The `hand` role on a `primary` pool with `slots` slots and no fallback.
fn routing(slots: u32) -> RoleRouting {
    let pool = RoutedPool {
        slots,
        provider: String::from("local"),
        fallback: None,
        caps: BTreeMap::new(),
        models: Vec::new(),
        has_bands: false,
    };
    RoleRouting {
        roles: vec![RoutedRole {
            name: String::from("hand"),
            kind: Some(String::from("hand")),
            status: String::from("active"),
            pool: Some(String::from("primary")),
            read_only: Some(false),
            model: None,
            models: Vec::new(),
        }],
        pools: BTreeMap::from([(String::from("primary"), pool)]),
    }
}

/// One free `primary` slot, a task send that creates a handle, a dispatch
/// that accepts it.
fn accept_one_task(host: &FakeHost) {
    host.reply("pool status", 0, "slot primary bridge slot-1 free\n");
    host.reply("task send", 0, "created hand a1b2c3d4\n");
    host.reply(
        "pool dispatch",
        0,
        "dispatched a1b2c3d4 bridge slot-1 model/default unit/task\n",
    );
}

fn job(id: &str, queued_at: u64) -> PoolJob {
    let mut job = PoolJob::from(JobSpec::new(id, "hand", "hand", "s", "b"));
    job.queued_at = queued_at;
    job
}

fn hold_until(job: &mut PoolJob, at: u64) {
    replay::stamp_dispatch_not_before(&mut job.payload, at);
}

fn ctx_for(dir: &std::path::Path, slots: u32) -> (Ctx, std::sync::Arc<FakeHost>) {
    let (ctx, host) = test_ctx(dir, PumpConfig::default());
    (ctx.with_routing(routing(slots)), host)
}

#[test]
fn a_future_deadline_occupies_no_capacity_and_a_due_job_dispatches() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = ctx_for(dir.path(), 1);
    accept_one_task(&host);
    let mut future = job("zz-future", 10);
    hold_until(&mut future, ctx.now() + 500);
    let due = job("aa-due", 20);
    let mut store = Store::default();
    store.jobs.insert("zz-future".into(), future);
    store.jobs.insert("aa-due".into(), due);

    assert_eq!(
        dispatch_pending(&ctx, &mut store).unwrap(),
        vec![String::from("aa-due")],
        "the future job is excluded before ranking, so the due job takes the one slot"
    );

    let future = &store.jobs["zz-future"];
    assert_eq!(future.status, JobStatus::Pending);
    assert_eq!(future.task, None, "no task was sent for the future job");
    assert_eq!(future.dispatched_at, None);
    assert_eq!(future.dispatch_refusals, 0, "no refusal was counted");
    assert_eq!(future.held, None);
    assert_eq!(
        host.commands()
            .iter()
            .filter(|command| command.contains("task send"))
            .count(),
        1,
        "only the due job's task was sent"
    );
}

#[test]
fn a_deadline_holds_a_job_only_until_exactly_its_second() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = ctx_for(dir.path(), 1);
    accept_one_task(&host);
    let mut late = job("late", ctx.now());
    hold_until(&mut late, ctx.now() + 10);
    let mut store = Store::default();
    store.jobs.insert("late".into(), late);

    assert!(dispatch_pending(&ctx, &mut store).unwrap().is_empty());
    assert_eq!(store.jobs["late"].status, JobStatus::Pending);

    host.advance(9);
    assert!(
        dispatch_pending(&ctx, &mut store).unwrap().is_empty(),
        "one second before the deadline it still waits"
    );

    host.advance(1);
    assert_eq!(
        dispatch_pending(&ctx, &mut store).unwrap(),
        vec![String::from("late")],
        "exactly at the deadline it dispatches"
    );
}

#[test]
fn a_reload_preserves_each_job_s_deadline_and_its_eligibility() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = ctx_for(dir.path(), 2);
    accept_one_task(&host);
    let start = ctx.now();
    let mut held = job("held", 1);
    hold_until(&mut held, start + 500);
    let mut ready = job("ready", 2);
    hold_until(&mut ready, start + 100);
    let mut state = State::default();
    state.jobs.insert("held".into(), held);
    state.jobs.insert("ready".into(), ready);
    let path = dir.path().join("pump.json");
    state.save(&path).unwrap();

    let mut reloaded = State::load(&path).unwrap();
    host.set_now(start + 99);
    assert!(
        dispatch_pending(&ctx, &mut reloaded).unwrap().is_empty(),
        "both reloaded jobs still wait before their deadlines"
    );
    host.set_now(start + 100);
    assert_eq!(
        dispatch_pending(&ctx, &mut reloaded).unwrap(),
        vec![String::from("ready")],
        "the reloaded job at its deadline dispatches"
    );
    assert_eq!(reloaded.jobs["held"].status, JobStatus::Pending);
    assert_eq!(reloaded.jobs["held"].dispatch_refusals, 0);
    host.set_now(start + 500);
    assert_eq!(
        dispatch_pending(&ctx, &mut reloaded).unwrap(),
        vec![String::from("held")],
        "the reloaded later deadline still releases exactly at its second"
    );
}
