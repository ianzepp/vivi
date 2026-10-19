use super::*;
use crate::mailspace::{Mailspace, SendRequest};

const DISPATCH: &str = r#"{"pool":"here","slot":"vivarium-here-slot-1","machine":"burgus","job":"3cc45261-3-a1","model":"zai/glm-5.3-flash","from":"mind","branch":"unit/3cc45261-3-a1","start":"1792421950abc","budget":1800,"at":1792421950,"read_only":false}"#;
const COLLECT: &str =
    r#"{"job":"3cc45261-3-a1","pool":"here","slot":"vivarium-here-slot-1","outcome":"failed","agent_rc":"1"}"#;

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
fn a_dispatched_task_reads_open_with_task_info() {
    let (_tmp, space, handle) = space_with_task();
    space
        .record_pool_note(&handle, "pool_dispatched", DISPATCH, "dispatched")
        .unwrap();
    let events = read(&space.dir.join("mail.sqlite"), 8).unwrap();
    assert_eq!(events.open.len(), 1);
    assert_eq!(events.open[0].task, handle);
    assert_eq!(events.open[0].record.model, "zai/glm-5.3-flash");
    assert_eq!(events.open[0].record.pool, "here");
    assert_eq!(events.open[0].record.slot, "vivarium-here-slot-1");
    let info = events.task_info.get(&handle).unwrap();
    assert_eq!(info.subject, "Fix the edge");
    assert_eq!(info.role, "hand");
    assert!(events.finished.is_empty());
}

#[test]
fn a_collected_dispatch_reads_finished_with_result_keys() {
    let (_tmp, space, handle) = space_with_task();
    space
        .record_pool_note(&handle, "pool_dispatched", DISPATCH, "dispatched")
        .unwrap();
    space
        .record_pool_note(&handle, "pool_collected", COLLECT, "failed")
        .unwrap();
    let events = read(&space.dir.join("mail.sqlite"), 8).unwrap();
    assert!(events.open.is_empty(), "the collect closes the dispatch");
    assert_eq!(events.finished.len(), 1);
    let done = &events.finished[0];
    assert_eq!(done.task, handle);
    assert_eq!(done.subject, "Fix the edge");
    assert_eq!(done.role, "hand");
    assert!(done.outcome.contains("failed"), "{}", done.outcome);
    assert!(done.outcome.contains("agent_rc=1"), "{}", done.outcome);
    assert_eq!(done.model, "zai/glm-5.3-flash");
    assert_eq!(done.pool, "here");
    assert_eq!(done.slot, "vivarium-here-slot-1");
    assert!(!done.read_only);
    assert!(!done.ended_at.is_empty());
}

#[test]
fn read_missing_db_is_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let events = read(&tmp.path().join("absent.sqlite"), 8).unwrap();
    assert!(events.open.is_empty());
    assert!(events.task_info.is_empty());
    assert!(events.finished.is_empty());
}
