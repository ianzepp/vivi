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
