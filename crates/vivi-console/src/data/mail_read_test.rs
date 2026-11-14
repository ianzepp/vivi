use super::*;
use crate::data::store::ReadStore;
use crate::pages::test_support::project_with_task;

#[test]
fn a_leading_front_matter_fence_is_dropped() {
    assert_eq!(strip_front("+++\na = 1\n+++\n\nbody\n"), "body\n");
    assert_eq!(strip_front("plain\n"), "plain\n");
    assert_eq!(strip_front("+++\nunclosed\n"), "+++\nunclosed\n");
    assert_eq!(strip_front("+++\n+++\n\nx"), "+++\n+++\n\nx");
}

#[test]
fn a_letter_file_outside_the_mailspace_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    for bad in ["../x", "/etc/hosts", "a/../../x"] {
        assert!(read_blob(dir.path(), bad).is_err(), "{bad}");
    }
}

#[test]
fn the_task_and_its_replies_read_back_read_only() {
    let project = project_with_task("Fix the edge case.");
    project.dispatch("job-1", 1_000);
    project.record(
        "pool_collected",
        r#"{"job":"job-1","slot":"slot-1"}"#,
        "pool result job-1: finished",
    );
    let store = ReadStore::open(&project.root).unwrap().unwrap();
    store
        .read(|conn| {
            let message_id: String = conn
                .query_row("SELECT message_id FROM mailspace_events LIMIT 1", [], |r| {
                    r.get(0)
                })
                .unwrap();
            let (content, task) = task_letter(conn, &project.root, &message_id)?.unwrap();
            assert!(task.text.contains("Fix the edge case."));
            assert!(
                task.text.starts_with("+++\n"),
                "the task keeps its front matter"
            );
            assert_eq!(task.subject, "Fix the edge");
            let replies = replies(conn, &project.root, &content)?;
            assert_eq!(replies.len(), 2);
            assert!(replies[0].text.starts_with("pool dispatch job-1"));
            assert!(replies[1].text.starts_with("pool result job-1"));
            assert!(replies.iter().all(|reply| !reply.text.contains("+++")));
            assert!(replies[0].at.is_some());
            assert_eq!(task_letter(conn, &project.root, "msg_nope")?, None);
            Ok(())
        })
        .unwrap();
}
