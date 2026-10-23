use super::*;
use crate::mailspace::SendRequest;

fn space_with_task(from: &str, to: &str) -> (tempfile::TempDir, Mailspace, String) {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = Mailspace::init(Some(tmp.path())).unwrap();
    space.add_identity("mind").unwrap();
    space.add_identity("hand").unwrap();
    let sent = space
        .send(SendRequest {
            from: from.into(),
            to: vec![to.into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "Fix the edge".into(),
            body: "do it\nfrom: main\nbranch: factory/x\n".into(),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap();
    let handle = sent.delivered[0].handle.clone();
    (tmp, space, handle)
}

#[test]
fn the_task_body_is_the_brief() {
    let (_tmp, space, handle) = space_with_task("mind", "hand");
    let task = space.pool_task(&handle).unwrap();
    assert_eq!(task.handle, handle);
    assert_eq!(task.subject, "Fix the edge");
    assert!(task.body.contains("branch: factory/x"));
}

#[test]
fn only_open_tasks_dispatch() {
    let (_tmp, space, handle) = space_with_task("mind", "hand");
    space
        .move_item("hand", &handle, "done", None, "task done", None)
        .unwrap();
    let err = space.pool_task(&handle).unwrap_err().to_string();
    assert!(err.contains("done or absorbed"), "{err}");
    assert!(space.pool_task("zzzzzzzz").is_err());
}

#[test]
fn a_note_lands_as_an_event_and_a_reply_and_never_moves_the_task() {
    let (_tmp, space, handle) = space_with_task("mind", "hand");
    space
        .record_pool_note(
            &handle,
            "pool_dispatched",
            r#"{"job":"j1"}"#,
            "dispatched to slot 1",
        )
        .unwrap();
    space
        .record_pool_note(
            &handle,
            "pool_collected",
            r#"{"outcome":"finished"}"#,
            "result: ok",
        )
        .unwrap();
    let events = space.pool_events().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event_type, "pool_dispatched");
    assert_eq!(events[0].handle, handle);
    assert_eq!(events[1].note, r#"{"outcome":"finished"}"#);
    let task = space.pool_task(&handle).unwrap();
    assert_eq!(task.subject, "Fix the edge", "still an open task");
    assert!(!space.is_task_done(&handle).unwrap());
}

#[test]
fn a_self_addressed_task_takes_notes_too() {
    let (_tmp, space, handle) = space_with_task("mind", "mind");
    space
        .record_pool_note(&handle, "pool_dispatched", "{}", "dispatched")
        .unwrap();
    assert_eq!(space.pool_events().unwrap().len(), 1);
}

#[test]
fn handles_resolve_for_matching() {
    let (_tmp, space, handle) = space_with_task("mind", "hand");
    assert_eq!(space.pool_handle(&handle).unwrap(), handle);
}

#[test]
fn the_addressee_is_the_role_the_task_copy_is_held_under() {
    let (_tmp, mut space, handle) = space_with_task("mind", "hand");
    let task = space.pool_task(&handle).unwrap();
    assert_eq!(task.addressee.as_deref(), Some("hand"));
    // A task whose recipient is not a known local role has no addressee.
    space.config.identities.retain(|role| role.name != "hand");
    let task = space.pool_task(&handle).unwrap();
    assert_eq!(task.addressee, None);
}

fn reply_on_task(space: &Mailspace, handle: &str, body: &str) {
    space
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["hand".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "attempt".into(),
            body: body.into(),
            role: "tasks".into(),
            kind: None,
            reply_to: Some(handle.into()),
            depends_on: Vec::new(),
        })
        .unwrap();
}

#[test]
fn a_task_with_no_attempt_replies_keeps_its_body() {
    let (_tmp, space, handle) = space_with_task("mind", "hand");
    let before = space.pool_task(&handle).unwrap().body;
    reply_on_task(&space, &handle, "an ordinary reply, not an attempt");
    let task = space.pool_task(&handle).unwrap();
    assert!(!task.body.contains("Earlier attempts"), "{}", task.body);
    assert_eq!(task.body, before);
}

#[test]
fn attempt_replies_are_appended_in_order() {
    let (_tmp, space, handle) = space_with_task("mind", "hand");
    reply_on_task(
        &space,
        &handle,
        &format!("{}\nfirst body", marked_line("pump-attempt:", "first")),
    );
    reply_on_task(
        &space,
        &handle,
        &format!("{}\nsecond body", marked_line("pump-attempt:", "second")),
    );
    let body = space.pool_task(&handle).unwrap().body;
    let heading = body.find("## Earlier attempts").expect("heading");
    let first = body.find("first body").expect("first");
    let second = body.find("second body").expect("second");
    assert!(body.starts_with("do it"), "{body}");
    assert!(heading < first && first < second, "{body}");
}

#[test]
fn the_attempt_section_is_capped() {
    let (_tmp, space, handle) = space_with_task("mind", "hand");
    reply_on_task(
        &space,
        &handle,
        &format!(
            "{}\nOLDEST\n{}",
            marked_line("pump-attempt:", "big"),
            "x".repeat(20_000)
        ),
    );
    reply_on_task(
        &space,
        &handle,
        &format!("{}\nNEWEST", marked_line("pump-attempt:", "last")),
    );
    let body = space.pool_task(&handle).unwrap().body;
    assert!(body.contains("[older attempts cut]"), "cut marker");
    assert!(body.contains("NEWEST"), "newest kept");
    assert!(!body.contains("OLDEST"), "oldest dropped");
    assert!(body.len() < 8 * 1024 + 512, "{}", body.len());
}

#[test]
fn only_the_newest_brief_is_the_body_with_attempts_after_it() {
    let (_tmp, space, handle) = space_with_task("mind", "hand");
    reply_on_task(
        &space,
        &handle,
        &format!("{}\nOLD BRIEF", marked_line("pump-brief:", "job-a")),
    );
    reply_on_task(
        &space,
        &handle,
        &format!("{}\nattempt body", marked_line("pump-attempt:", "first")),
    );
    reply_on_task(
        &space,
        &handle,
        &format!(
            "{}\nWRITE SCOPE and envelope",
            marked_line("pump-brief:", "job-b")
        ),
    );
    let body = space.pool_task(&handle).unwrap().body;
    assert!(body.starts_with("pump-brief: t="), "{body}");
    assert!(body.lines().next().unwrap().ends_with("job-b"), "{body}");
    assert!(!body.contains("OLD BRIEF"), "{body}");
    assert!(!body.contains("do it"), "task body not repeated: {body}");
    let heading = body.find("## Earlier attempts").expect("heading");
    assert!(body.find("WRITE SCOPE").unwrap() < heading, "{body}");
    assert!(body.contains("attempt body"), "{body}");
}

#[test]
fn a_task_with_no_brief_reply_keeps_its_body() {
    let (_tmp, space, handle) = space_with_task("mind", "hand");
    reply_on_task(&space, &handle, "not a pump-brief: reply");
    let body = space.pool_task(&handle).unwrap().body;
    assert!(body.starts_with("do it"), "{body}");
}

#[test]
fn same_second_attempts_come_back_in_posting_order() {
    for _ in 0..20 {
        let (_tmp, space, handle) = space_with_task("mind", "hand");
        for n in 1..=3 {
            reply_on_task(
                &space,
                &handle,
                &format!(
                    "{}\nbody-{n}",
                    marked_line("pump-attempt:", &format!("a{n}"))
                ),
            );
        }
        let body = space.pool_task(&handle).unwrap().body;
        let at = |n: u32| body.find(&format!("body-{n}")).expect("attempt");
        assert!(at(1) < at(2) && at(2) < at(3), "{body}");
    }
}

#[test]
fn the_newest_brief_wins_within_one_second() {
    for _ in 0..20 {
        let (_tmp, space, handle) = space_with_task("mind", "hand");
        for job in ["job-a", "job-b", "job-c"] {
            reply_on_task(
                &space,
                &handle,
                &format!("{}\nBRIEF {job}", marked_line("pump-brief:", job)),
            );
        }
        let body = space.pool_task(&handle).unwrap().body;
        assert!(body.contains("BRIEF job-c"), "{body}");
        assert!(!body.contains("BRIEF job-a"), "{body}");
    }
}

#[test]
fn stamps_strictly_increase_and_payload_drops_them() {
    let a = marked_line("pump-brief:", "j1");
    let b = marked_line("pump-brief:", "j1");
    assert_ne!(a, b);
    assert_eq!(marked_payload(&a, "pump-brief:"), Some("j1"));
    assert_eq!(marked_payload("pump-brief: j1", "pump-brief:"), Some("j1"));
    assert_eq!(marked_payload("other: j1", "pump-brief:"), None);
}
