use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::*;
use crate::mailspace::SendRequest;
use crate::pool::fake_provider::{Fake, claimed, pid};
use crate::pool::record::{self, DispatchRecord};
use crate::pool::work::{epoch_secs, open_dispatches};

fn space() -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = Mailspace::init(Some(tmp.path())).unwrap();
    space.add_identity("mind").unwrap();
    space.add_identity("hand").unwrap();
    (tmp, space)
}

fn task(space: &mut Mailspace, subject: &str) -> String {
    space
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["hand".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: crate::letter::fixture::task_body("do it\n"),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap()
        .delivered[0]
        .handle
        .clone()
}

fn dispatched(space: &Mailspace, task: &str, pool: &str, machine: &str) {
    let record = DispatchRecord {
        target: None,
        repository: None,
        pool: pool.into(),
        need: None,
        kind: None,
        slot: format!("ms-{pool}-slot-1"),
        machine: machine.into(),
        job: format!("job-{pool}"),
        model: "m/x".into(),
        from: "main".into(),
        branch: "factory/a".into(),
        start: "f".repeat(40),
        with: Vec::new(),
        budget: 600,
        at: epoch_secs() - 100,
        read_only: false,
        hardware: String::new(),
        abandoned: 0,
        abandoned_cost_micros: 0,
    };
    space
        .record_pool_note(
            task,
            "pool_dispatched",
            &record::dispatch_note(&record),
            "dispatched",
        )
        .unwrap();
}

fn finished(job: &str) -> String {
    format!(
        "@@vivi-pool result\njob={job}\nstate=done\npush=pushed\nbranch=factory/a\nsha=0123456789abcdef0123456789abcdef01234567\nstart=fedcba9876543210\nagent_rc=0\nagent_s=1\njob_s=2\n@@vivi-pool receipt\nok\n@@vivi-pool stderr\n\n@@vivi-pool log\n\n"
    )
}

fn quick() -> Pacing {
    Pacing {
        poll: Duration::ZERO,
        ready_timeout: Duration::ZERO,
        stop_timeout: Duration::from_millis(50),
        start_grace: Duration::ZERO,
    }
}

/// Count this caller in and wait until `n` callers are inside at once. Only a
/// failing run reaches the deadline.
fn rendezvous(inside: &AtomicUsize, n: usize) -> bool {
    inside.fetch_add(1, Ordering::SeqCst);
    let deadline = Instant::now() + Duration::from_secs(20);
    while inside.load(Ordering::SeqCst) < n {
        if Instant::now() > deadline {
            return false;
        }
        std::thread::yield_now();
    }
    true
}

/// Two pools, `a` and `b`, each holding one finished running slot for its
/// task; `b`'s dispatch was opened first. The opener builds each pool's fake
/// on the pool's own thread.
fn world() -> (tempfile::TempDir, Mailspace, String, String) {
    let (tmp, mut space) = space();
    let (ta, tb) = (task(&mut space, "for a"), task(&mut space, "for b"));
    dispatched(&space, &tb, "b", "mb");
    dispatched(&space, &ta, "a", "ma");
    (tmp, space, ta, tb)
}

#[test]
fn pools_are_reconciled_at_the_same_time_and_merged_in_dispatch_order() {
    let (_tmp, space, ta, tb) = world();
    let inside = Arc::new(AtomicUsize::new(0));
    let (task_a, task_b) = (ta.clone(), tb.clone());
    let open = {
        let inside = Arc::clone(&inside);
        move |_: &Mailspace, id: &PoolId| -> Result<Box<dyn PoolProvider>, VivariumError> {
            let (machine, task) = match id.pool.as_str() {
                "a" => ("ma", &task_a),
                _ => ("mb", &task_b),
            };
            let fake = Fake::default();
            fake.add(claimed(
                machine,
                &format!("ms-{}-slot-1", id.pool),
                "started",
                &id.pool,
                task,
            ));
            fake.exec_out
                .borrow_mut()
                .insert(machine.into(), finished(&format!("job-{}", id.pool)));
            let inside = Arc::clone(&inside);
            *fake.exec_hook.borrow_mut() = Some(Box::new(move |_, _| {
                // Both pools must be polling at once for this to pass.
                (!rendezvous(&inside, 2)).then(String::new)
            }));
            Ok(Box::new(fake))
        }
    };
    let (a, b) = (pid("a"), pid("b"));
    let report = reconcile_pools(&space, &[&a, &b], true, &quick(), &open).unwrap();
    assert!(report.error().is_none(), "{:?}", report.error());
    let seen: Vec<(&str, &str)> = report
        .items
        .iter()
        .map(|l| (l.pool.as_str(), l.state))
        .collect();
    // `b` was opened first, so its line comes first, as in a serial pass.
    assert_eq!(seen, [("b", "finished"), ("a", "finished")], "{report:?}");
    assert_eq!(report.items[0].task, tb);
    assert_eq!(report.items[1].task, ta);
    assert_eq!(report.out.len(), 2);
    assert!(!report.idle);
    assert!(open_dispatches(&space).unwrap().is_empty());
}

#[test]
fn the_merged_report_matches_a_serial_pass_over_the_same_pools() {
    let (_tmp, space, _ta, _tb) = world();
    let opened = open_dispatches(&space).unwrap();
    let open = |_: &Mailspace, _: &PoolId| -> Result<Box<dyn PoolProvider>, VivariumError> {
        Ok(Box::new(Fake::default()))
    };
    let (a, b) = (pid("a"), pid("b"));
    let report = reconcile_pools(&space, &[&a, &b], true, &quick(), &open).unwrap();
    // Neither pool has the machine: both dispatches settle as lost.
    let order: Vec<&str> = report.items.iter().map(|l| l.task.as_str()).collect();
    let want: Vec<&str> = opened.iter().map(|d| d.task.as_str()).collect();
    assert_eq!(order, want, "lines follow the order the dispatches opened");
    assert!(report.items.iter().all(|l| l.state == "lost"));
    assert_eq!(
        report.out,
        report.items.iter().map(work::line_text).collect::<Vec<_>>()
    );
    assert_eq!(report.round, report.items.len());
}

#[test]
fn a_pool_that_is_gone_is_closed_as_lost_only_when_every_pool_is_covered() {
    let (_tmp, mut space) = space();
    let orphan = task(&mut space, "orphan");
    dispatched(&space, &orphan, "removed", "mx");
    let open = |_: &Mailspace, _: &PoolId| -> Result<Box<dyn PoolProvider>, VivariumError> {
        Ok(Box::new(Fake::default()))
    };
    let (a, b) = (pid("a"), pid("b"));
    let one_pool = reconcile_pools(&space, &[&a, &b], false, &quick(), &open).unwrap();
    assert!(one_pool.idle && one_pool.items.is_empty(), "{one_pool:?}");
    assert_eq!(open_dispatches(&space).unwrap().len(), 1, "left open");
    let every = reconcile_pools(&space, &[&a, &b], true, &quick(), &open).unwrap();
    assert_eq!(every.items.len(), 1, "{every:?}");
    assert_eq!(every.items[0].state, "lost");
    assert_eq!(every.items[0].pool, "removed");
    assert!(open_dispatches(&space).unwrap().is_empty());
}

#[test]
fn nothing_open_is_idle_like_a_serial_pass() {
    let (_tmp, space) = space();
    let open = |_: &Mailspace, _: &PoolId| -> Result<Box<dyn PoolProvider>, VivariumError> {
        Ok(Box::new(Fake::default()))
    };
    let (a, b) = (pid("a"), pid("b"));
    let report = reconcile_pools(&space, &[&a, &b], true, &quick(), &open).unwrap();
    assert!(report.idle && report.items.is_empty());
    assert_eq!(report.out, ["no open dispatches"]);
}

#[test]
fn a_pool_that_cannot_be_opened_fails_the_reconcile_after_the_others_finish() {
    let (_tmp, space, _ta, _tb) = world();
    let open = |_: &Mailspace, id: &PoolId| -> Result<Box<dyn PoolProvider>, VivariumError> {
        if id.pool == "a" {
            return Err(VivariumError::Message("pool a is unreachable".into()));
        }
        Ok(Box::new(Fake::default()))
    };
    let (a, b) = (pid("a"), pid("b"));
    let err = reconcile_pools(&space, &[&a, &b], true, &quick(), &open).unwrap_err();
    assert!(err.to_string().contains("pool a is unreachable"), "{err}");
    let left = open_dispatches(&space).unwrap();
    assert_eq!(left.len(), 1, "pool b's dispatch was still settled");
    assert_eq!(left[0].record.pool, "a");
}
