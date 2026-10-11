use super::*;
use crate::mailspace::SendRequest;
use crate::pool::collect::ErrorWatch;
use crate::pool::fake_provider::{Fake, claimed};

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
    let report = reconcile(&space, &fake, "p", &quick()).unwrap();
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
    let again = reconcile(&space, &fake, "p", &quick()).unwrap();
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
        &Fake::default(),
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
        &fake,
        &[],
        &options,
        &mut ErrorWatch::default(),
        &mut |_| {},
    )
    .unwrap_err();
    assert!(none.to_string().contains("no open dispatches to wait for"));
    let named = wait(
        &space,
        &fake,
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
        &fake,
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
        &fake,
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
    wait(&space, &fake, &[task], &options, &mut watch, &mut |n| {
        notices.push(n.to_string());
    })
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
    let report = reconcile(&space, &Fake::default(), "p", &quick()).unwrap();
    assert_eq!(report.items.len(), 1);
    assert_eq!(report.items[0].state, "lost");
    assert!(!report.idle);
    let again = reconcile(&space, &Fake::default(), "p", &quick()).unwrap();
    assert!(again.idle && again.items.is_empty());
}
