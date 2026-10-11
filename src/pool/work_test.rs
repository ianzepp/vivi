use super::*;
use crate::mailspace::SendRequest;
use crate::pool::collect::ErrorWatch;
use crate::pool::fake_provider::{Fake, claimed, pid};
use crate::pool::pools::PoolSet;
use crate::pool::slot::PoolId;

fn space_with_task() -> (tempfile::TempDir, Mailspace, String) {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = Mailspace::init(Some(tmp.path())).unwrap();
    space.add_identity("mind").unwrap();
    space.add_identity("hand").unwrap();
    let sent = space
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["hand".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "Fix the edge".into(),
            body: "do it\n".into(),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap();
    let handle = sent.delivered[0].handle.clone();
    (tmp, space, handle)
}

fn dispatched(space: &Mailspace, task: &str, machine: &str) {
    let record = record::DispatchRecord {
        pool: "p".into(),
        slot: "p-slot-1".into(),
        machine: machine.into(),
        job: "j1".into(),
        model: "m/x".into(),
        from: "main".into(),
        branch: "factory/a".into(),
        start: "f".repeat(40),
        budget: 600,
        at: epoch_secs() - 100,
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

fn quick() -> Pacing {
    Pacing {
        poll: Duration::ZERO,
        ready_timeout: Duration::ZERO,
        stop_timeout: Duration::from_millis(50),
        start_grace: Duration::ZERO,
    }
}

#[test]
fn reconcile_closes_a_dispatch_whose_machine_no_longer_exists() {
    let (_tmp, space, task) = space_with_task();
    dispatched(&space, &task, "m-gone");
    let fake = Fake::default();
    let report = reconcile(&space, &PoolSet::single(&pid("p"), &fake), &quick()).unwrap();
    assert!(
        report
            .out
            .iter()
            .any(|l| l.contains("lost") && l.contains(&task)),
        "{:?}",
        report.out
    );
    assert!(report.error().is_none(), "{:?}", report.error());
    assert!(
        open_dispatches(&space).unwrap().is_empty(),
        "the dispatch is closed"
    );
    let events = space.pool_events().unwrap();
    let last = events.last().unwrap();
    assert_eq!(last.event_type, "pool_collected");
    assert!(last.note.contains("\"outcome\":\"lost\""), "{}", last.note);
    // The task can be dispatched again.
    assert!(space.pool_task(&task).is_ok());
    let again = reconcile(&space, &PoolSet::single(&pid("p"), &fake), &quick()).unwrap();
    assert_eq!(again.out, ["no open dispatches"]);
}

#[test]
fn wait_reports_a_vanished_machine_as_lost_instead_of_an_error() {
    let (_tmp, space, task) = space_with_task();
    dispatched(&space, &task, "m-gone");
    let options = WaitOptions {
        limit: Duration::from_millis(200),
        poll: Duration::ZERO,
        pacing: quick(),
        heartbeat: Duration::from_hours(1),
        cancelled: &|| false,
    };
    let waited = wait(
        &space,
        &PoolSet::single(&pid("p"), &Fake::default()),
        &[task.clone()],
        &options,
        &mut ErrorWatch::default(),
        &mut |_| {},
    )
    .unwrap();
    let Waited::Settled(lines) = waited else {
        panic!("expected a settled line");
    };
    assert_eq!(lines[0].state, "lost");
}

#[test]
fn wait_needs_something_to_wait_for_and_says_what_is_missing() {
    let (_tmp, space, task) = space_with_task();
    let options = WaitOptions {
        limit: Duration::from_millis(50),
        poll: Duration::ZERO,
        pacing: quick(),
        heartbeat: Duration::from_hours(1),
        cancelled: &|| false,
    };
    let fake = Fake::default();
    let none = wait(
        &space,
        &PoolSet::single(&pid("p"), &fake),
        &[],
        &options,
        &mut ErrorWatch::default(),
        &mut |_| {},
    )
    .unwrap_err();
    assert!(none.to_string().contains("no open dispatches to wait for"));
    let named = wait(
        &space,
        &PoolSet::single(&pid("p"), &fake),
        &[task],
        &options,
        &mut ErrorWatch::default(),
        &mut |_| {},
    )
    .unwrap_err();
    assert!(
        named.to_string().contains("no open dispatch for:"),
        "{named}"
    );
}

#[test]
fn wait_gives_up_at_its_limit_and_stops_when_cancelled() {
    let (_tmp, space, task) = space_with_task();
    dispatched(&space, &task, "m1");
    let fake = Fake::default();
    fake.add(claimed("m1", "p-slot-1", "started", "p", &task));
    let mut options = WaitOptions {
        limit: Duration::from_millis(30),
        poll: Duration::from_millis(1),
        pacing: quick(),
        heartbeat: Duration::from_hours(1),
        cancelled: &|| false,
    };
    let late = wait(
        &space,
        &PoolSet::single(&pid("p"), &fake),
        &[task.clone()],
        &options,
        &mut ErrorWatch::default(),
        &mut |_| {},
    )
    .unwrap_err();
    assert!(
        late.to_string().contains("nothing finished within"),
        "{late}"
    );
    options.cancelled = &|| true;
    let stopped = wait(
        &space,
        &PoolSet::single(&pid("p"), &fake),
        &[task],
        &options,
        &mut ErrorWatch::default(),
        &mut |_| {},
    )
    .unwrap_err();
    assert!(stopped.to_string().contains("interrupted"), "{stopped}");
}

fn running_fake(task: &str) -> Fake {
    let fake = Fake::default();
    fake.add(claimed("m1", "p-slot-1", "started", "p", task));
    fake
}

fn wait_notices(heartbeat: Duration) -> (Vec<String>, ErrorWatch) {
    let (_tmp, space, task) = space_with_task();
    dispatched(&space, &task, "m1");
    let fake = running_fake(&task);
    let options = WaitOptions {
        limit: Duration::from_millis(120),
        poll: Duration::from_millis(5),
        pacing: quick(),
        heartbeat,
        cancelled: &|| false,
    };
    let mut watch = ErrorWatch::default();
    let mut notices = Vec::new();
    wait(
        &space,
        &PoolSet::single(&pid("p"), &fake),
        &[task],
        &options,
        &mut watch,
        &mut |n| {
            notices.push(n.to_string());
        },
    )
    .unwrap_err();
    (notices, watch)
}

#[test]
fn wait_announces_itself_and_beats_while_nothing_settles() {
    let (notices, _) = wait_notices(Duration::from_millis(20));
    assert!(
        notices[0].starts_with("waiting for 1 dispatch(es): "),
        "{notices:?}"
    );
    assert!(
        notices[0].ends_with("(checks every 0s, gives up after 0s)"),
        "{notices:?}"
    );
    let beats: Vec<&String> = notices
        .iter()
        .filter(|n| n.starts_with("waiting  "))
        .collect();
    assert!(beats.len() >= 2, "{notices:?}");
    assert!(beats[0].contains(" on p-slot-1  running "), "{beats:?}");
    assert!(beats[0].ends_with("s budget"), "{beats:?}");
}

#[test]
fn a_wait_shorter_than_the_heartbeat_prints_no_heartbeat() {
    let (notices, _) = wait_notices(Duration::from_secs(30));
    assert_eq!(notices.len(), 1, "only the start line: {notices:?}");
}

#[test]
fn the_heartbeat_text_names_what_is_waited_on() {
    let running = RoundLine {
        task: "3f2a".into(),
        pool: "p".into(),
        slot: "p-slot-2".into(),
        state: "running",
        detail: "150s of 1800s".into(),
    };
    assert_eq!(
        beat_text(std::slice::from_ref(&running)),
        "waiting  3f2a on p-slot-2  running 150s of 1800s budget"
    );
    let broken = RoundLine {
        task: "9c1d".into(),
        pool: "p".into(),
        slot: "p-slot-1".into(),
        state: "error",
        detail: "Fly unreachable".into(),
    };
    assert_eq!(
        beat_text(&[running, broken]),
        "waiting  3f2a on p-slot-2  running 150s of 1800s budget; 9c1d on p-slot-1  cannot poll: Fly unreachable"
    );
}

#[test]
fn reconcile_keeps_each_line_for_porcelain() {
    let (_tmp, space, task) = space_with_task();
    dispatched(&space, &task, "m-gone");
    let report = reconcile(
        &space,
        &PoolSet::single(&pid("p"), &Fake::default()),
        &quick(),
    )
    .unwrap();
    assert_eq!(report.items.len(), 1);
    assert_eq!(report.items[0].state, "lost");
    assert!(!report.idle);
    let again = reconcile(
        &space,
        &PoolSet::single(&pid("p"), &Fake::default()),
        &quick(),
    )
    .unwrap();
    assert!(again.idle && again.items.is_empty());
}

fn send_task(space: &mut Mailspace, subject: &str) -> String {
    space
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["hand".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: "do it\n".into(),
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

fn dispatched_to(space: &Mailspace, task: &str, pool: &str, machine: &str) {
    let record = record::DispatchRecord {
        pool: pool.into(),
        slot: format!("ms-{pool}-slot-1"),
        machine: machine.into(),
        job: format!("job-{pool}"),
        model: "m/x".into(),
        from: "main".into(),
        branch: "factory/a".into(),
        start: "f".repeat(40),
        budget: 600,
        at: epoch_secs() - 100,
    };
    let note = record::dispatch_note(&record);
    space
        .record_pool_note(task, "pool_dispatched", &note, "dispatched")
        .unwrap();
}

/// A finished job's collect output for `job`.
fn finished(job: &str) -> String {
    format!(
        "@@vivi-pool result\njob={job}\nstate=done\npush=pushed\nbranch=factory/a\nsha=0123456789abcdef0123456789abcdef01234567\nstart=fedcba9876543210\nagent_rc=0\nagent_s=1\njob_s=2\n@@vivi-pool receipt\nok\n@@vivi-pool stderr\n\n@@vivi-pool log\n\n"
    )
}

/// Two pools of one mailspace, each with one running slot that holds a task.
fn two_pools(space: &mut Mailspace) -> (Fake, Fake, String, String) {
    let (a, b) = (Fake::default(), Fake::default());
    let (ta, tb) = (send_task(space, "for a"), send_task(space, "for b"));
    a.add(claimed("ma", "ms-a-slot-1", "started", "a", &ta));
    b.add(claimed("mb", "ms-b-slot-1", "started", "b", &tb));
    a.exec_out
        .borrow_mut()
        .insert("ma".into(), finished("job-a"));
    b.exec_out
        .borrow_mut()
        .insert("mb".into(), finished("job-b"));
    dispatched_to(space, &ta, "a", "ma");
    dispatched_to(space, &tb, "b", "mb");
    (a, b, ta, tb)
}

fn two_pool_set<'a>(
    ids: &'a (PoolId, PoolId),
    fakes: (&'a Fake, &'a Fake),
    all: bool,
) -> PoolSet<'a> {
    let member = |id: &'a PoolId, fake: &'a Fake| crate::pool::pools::Member { id, provider: fake };
    PoolSet {
        members: vec![member(&ids.0, fakes.0), member(&ids.1, fakes.1)],
        all,
    }
}

fn options() -> WaitOptions<'static> {
    WaitOptions {
        limit: Duration::from_millis(500),
        poll: Duration::ZERO,
        pacing: quick(),
        heartbeat: Duration::from_hours(1),
        cancelled: &|| false,
    }
}

#[test]
fn one_wait_without_a_pool_collects_from_both_pools() {
    let (_tmp, mut space, _first) = space_with_task();
    let (a, b, ta, tb) = two_pools(&mut space);
    let ids = (pid("a"), pid("b"));
    let set = two_pool_set(&ids, (&a, &b), true);
    let waited = wait(
        &space,
        &set,
        &[],
        &options(),
        &mut ErrorWatch::default(),
        &mut |_| {},
    )
    .unwrap();
    let Waited::Settled(lines) = waited else {
        panic!("expected settled lines");
    };
    let mut seen: Vec<(String, String, &str)> = lines
        .iter()
        .map(|l| (l.task.clone(), l.pool.clone(), l.state))
        .collect();
    seen.sort();
    let mut want = vec![
        (ta.clone(), "a".to_string(), "finished"),
        (tb.clone(), "b".to_string(), "finished"),
    ];
    want.sort();
    assert_eq!(seen, want);
    assert!(open_dispatches(&space).unwrap().is_empty());
    assert!(
        a.meta("ma", crate::pool::slot::META_TASK).is_none(),
        "a's slot is freed"
    );
    assert!(
        b.meta("mb", crate::pool::slot::META_TASK).is_none(),
        "b's slot is freed"
    );
    assert_eq!(a.state_of("ma"), "stopped");
    assert_eq!(b.state_of("mb"), "stopped");
}

#[test]
fn a_wait_for_one_pool_leaves_the_other_pools_dispatch_open() {
    let (_tmp, mut space, _first) = space_with_task();
    let (a, b, _ta, tb) = two_pools(&mut space);
    let ids = (pid("a"), pid("b"));
    let only_a = PoolSet::single(&ids.0, &a);
    let waited = wait(
        &space,
        &only_a,
        &[],
        &options(),
        &mut ErrorWatch::default(),
        &mut |_| {},
    )
    .unwrap();
    let Waited::Settled(lines) = waited else {
        panic!("expected settled lines");
    };
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].pool, "a");
    let open = open_dispatches(&space).unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].task, tb);
    assert_eq!(b.calls_to("exec"), 0, "pool b was not asked");
}

#[test]
fn reconcile_covers_every_pool_and_each_dispatch_uses_its_own_provider() {
    let (_tmp, mut space, _first) = space_with_task();
    let (a, b, _ta, _tb) = two_pools(&mut space);
    let ids = (pid("a"), pid("b"));
    let set = two_pool_set(&ids, (&a, &b), true);
    let report = reconcile(&space, &set, &quick()).unwrap();
    let pools: Vec<&str> = report.items.iter().map(|l| l.pool.as_str()).collect();
    assert!(pools.contains(&"a") && pools.contains(&"b"), "{pools:?}");
    assert!(report.error().is_none(), "{:?}", report.error());
    assert!(open_dispatches(&space).unwrap().is_empty());
    assert_eq!(a.calls_to("exec"), 1);
    assert_eq!(b.calls_to("exec"), 1);
}

#[test]
fn reconcile_of_one_pool_does_not_touch_the_other() {
    let (_tmp, mut space, _first) = space_with_task();
    let (a, b, _ta, _tb) = two_pools(&mut space);
    let ids = (pid("a"), pid("b"));
    let only_b = PoolSet::single(&ids.1, &b);
    reconcile(&space, &only_b, &quick()).unwrap();
    let open = open_dispatches(&space).unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].record.pool, "a");
    assert_eq!(a.calls_to("exec"), 0);
}

#[test]
fn a_dispatch_whose_pool_is_gone_is_reported_by_wait_and_closed_lost_by_reconcile() {
    let (_tmp, mut space, _first) = space_with_task();
    let task = send_task(&mut space, "orphan");
    dispatched_to(&space, &task, "removed", "mx");
    let live = Fake::default();
    let id = pid("a");
    let everything = PoolSet {
        members: PoolSet::single(&id, &live).members,
        all: true,
    };
    // wait: a clear error line, nothing crashes, the dispatch stays open.
    let mut watch = ErrorWatch::default();
    let mut said = Vec::new();
    let short = WaitOptions {
        limit: Duration::from_millis(20),
        ..options()
    };
    let failed = wait(&space, &everything, &[], &short, &mut watch, &mut |n| {
        said.push(n.to_string())
    });
    assert!(
        failed
            .unwrap_err()
            .to_string()
            .contains("is not in .vivi/mailspace.toml")
    );
    assert!(
        said.iter()
            .any(|n| n.contains("pool \"removed\" is not in .vivi/mailspace.toml")),
        "{said:?}"
    );
    assert_eq!(watch.issues()[0].pool.as_deref(), Some("removed"));
    assert_eq!(open_dispatches(&space).unwrap().len(), 1);
    // reconcile: recorded as lost and closed.
    let report = reconcile(&space, &everything, &quick()).unwrap();
    assert_eq!(report.items[0].state, "lost");
    assert!(report.items[0].detail.contains("closed as lost"));
    assert!(report.error().is_none());
    assert!(open_dispatches(&space).unwrap().is_empty());
    let last = space.pool_events().unwrap().pop().unwrap();
    assert!(
        last.note.contains("\"outcome\":\"lost\"") && last.note.contains("\"pool\":\"removed\""),
        "{}",
        last.note
    );
}

#[test]
fn a_dispatch_made_before_pools_were_named_reads_as_a_gone_pool() {
    let (_tmp, space, task) = space_with_task();
    // The old record had no `pool` field at all.
    let old = r#"{"slot":"x-slot-1","machine":"m1","job":"j","model":"m","from":"main","branch":"b","start":"s","budget":600,"at":1}"#;
    space
        .record_pool_note(&task, "pool_dispatched", old, "old")
        .unwrap();
    let open = open_dispatches(&space).unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].record.pool, "");
    let id = pid("a");
    let fake = Fake::default();
    let set = PoolSet {
        members: PoolSet::single(&id, &fake).members,
        all: true,
    };
    let report = reconcile(&space, &set, &quick()).unwrap();
    assert!(
        report.items[0].detail.contains("before pools were named"),
        "{:?}",
        report.items
    );
    assert!(open_dispatches(&space).unwrap().is_empty());
}
