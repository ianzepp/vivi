use std::collections::BTreeMap;

use super::*;
use crate::pool::collect::{Collected, JobResult};

fn record(job: &str) -> DispatchRecord {
    DispatchRecord {
        pool: "p".into(),
        slot: "p-slot-1".into(),
        machine: "m1".into(),
        job: job.into(),
        model: "m/x".into(),
        from: "main".into(),
        branch: "factory/a".into(),
        start: "a".repeat(40),
        budget: 600,
        at: 1000,
        read_only: false,
    }
}

fn event(handle: &str, kind: &str, note: String) -> PoolEventRow {
    PoolEventRow {
        handle: handle.into(),
        event_type: kind.into(),
        note,
    }
}

fn dispatched(handle: &str, job: &str) -> PoolEventRow {
    event(handle, "pool_dispatched", dispatch_note(&record(job)))
}

#[test]
fn a_dispatch_is_open_until_a_collected_event_follows() {
    let events = vec![
        dispatched("aaaa1111", "j1"),
        dispatched("bbbb2222", "j2"),
        event("aaaa1111", "pool_collected", "{}".into()),
    ];
    let open = fold_open(&events);
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].task, "bbbb2222");
    assert_eq!(open[0].record.job, "j2");
}

#[test]
fn a_task_can_be_dispatched_again_after_collection() {
    let events = vec![
        dispatched("aaaa1111", "j1"),
        event("aaaa1111", "pool_collected", "{}".into()),
        dispatched("aaaa1111", "j2"),
    ];
    let open = fold_open(&events);
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].record.job, "j2");
}

#[test]
fn unparseable_notes_are_not_dispatches() {
    let events = vec![event("aaaa1111", "pool_dispatched", "not json".into())];
    assert!(fold_open(&events).is_empty());
}

#[test]
fn dispatch_note_round_trips() {
    let r = record("j1");
    let back: DispatchRecord = serde_json::from_str(&dispatch_note(&r)).unwrap();
    assert_eq!(back, r);
    assert!(dispatch_text(&r).contains("branch factory/a"));
}

fn finished() -> Outcome {
    let fields = BTreeMap::from([
        ("job".to_string(), "j1".to_string()),
        ("push".to_string(), "pushed".to_string()),
        ("branch".to_string(), "factory/a".to_string()),
        ("sha".to_string(), "b".repeat(40)),
        ("agent_rc".to_string(), "0".to_string()),
    ]);
    Outcome::Finished(Collected {
        result: JobResult { fields },
        receipt: "did the thing\n".into(),
        stderr: String::new(),
        log: "from main at abc\n".into(),
    })
}

#[test]
fn collected_records_carry_the_result_and_the_receipt() {
    let d = OpenDispatch {
        task: "aaaa1111".into(),
        record: record("j1"),
    };
    let note: serde_json::Value = serde_json::from_str(&collected_note(&d, &finished())).unwrap();
    assert_eq!(note["outcome"], "finished");
    assert_eq!(note["push"], "pushed");
    assert_eq!(note["job"], "j1");
    let text = collected_text(&d, &finished());
    assert!(text.contains("pool result j1: finished"));
    assert!(text.contains("pushed factory/a bbbbbbbbbb"));
    assert!(text.contains("--- receipt\ndid the thing"));
    assert!(text.contains("--- runner log tail"));
    assert!(!text.contains("--- stderr"), "empty sections are left out");
}

#[test]
fn timeout_and_lost_say_what_happened() {
    let d = OpenDispatch {
        task: "aaaa1111".into(),
        record: record("j1"),
    };
    assert!(collected_text(&d, &Outcome::Timeout).contains("slot was stopped"));
    assert!(collected_text(&d, &Outcome::Lost).contains("without writing a result"));
    let note: serde_json::Value =
        serde_json::from_str(&collected_note(&d, &Outcome::Timeout)).unwrap();
    assert_eq!(note["outcome"], "timeout");
}

fn read_only_record(job: &str) -> DispatchRecord {
    DispatchRecord {
        branch: String::new(),
        read_only: true,
        ..record(job)
    }
}

fn read_only_outcome(extra: &[(&str, &str)]) -> Outcome {
    let mut fields = BTreeMap::from([
        ("job".to_string(), "j1".to_string()),
        ("branch".to_string(), String::new()),
        ("sha".to_string(), String::new()),
        ("agent_rc".to_string(), "0".to_string()),
        ("agent_s".to_string(), "3".to_string()),
        ("job_s".to_string(), "5".to_string()),
        ("start".to_string(), "c".repeat(40)),
    ]);
    fields.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    Outcome::Finished(Collected {
        result: JobResult { fields },
        receipt: "read it all\n".into(),
        stderr: String::new(),
        log: String::new(),
    })
}

#[test]
fn a_record_made_before_read_only_jobs_reads_as_a_normal_dispatch() {
    let mut value: serde_json::Value = serde_json::from_str(&dispatch_note(&record("j1"))).unwrap();
    value.as_object_mut().unwrap().remove("read_only");
    let back: DispatchRecord = serde_json::from_value(value).unwrap();
    assert!(!back.read_only);
}

#[test]
fn a_read_only_dispatch_round_trips_and_its_text_says_so() {
    let r = read_only_record("j1");
    let back: DispatchRecord = serde_json::from_str(&dispatch_note(&r)).unwrap();
    assert_eq!(back, r);
    let text = dispatch_text(&r);
    assert!(
        text.contains("read-only (nothing is committed or pushed)"),
        "{text}"
    );
    assert!(!text.contains("branch"), "{text}");
}

#[test]
fn an_unchanged_read_only_result_records_the_outcome_and_the_receipt() {
    let d = OpenDispatch {
        task: "aaaa1111".into(),
        record: read_only_record("j1"),
    };
    let outcome = read_only_outcome(&[("push", "read_only_unchanged")]);
    let note: serde_json::Value = serde_json::from_str(&collected_note(&d, &outcome)).unwrap();
    assert_eq!(note["push"], "read_only_unchanged");
    assert_eq!(note["read_only"], true);
    assert!(note.get("discarded").is_none());
    let text = collected_text(&d, &outcome);
    assert!(text.contains("from main  read-only"), "{text}");
    assert!(text.contains("result: read_only_unchanged"), "{text}");
    assert!(!text.contains("push:"), "{text}");
    assert!(!text.contains("discarded paths"), "{text}");
    assert!(text.contains("--- receipt\nread it all"), "{text}");
}

#[test]
fn a_discarding_read_only_result_lists_the_paths_in_order() {
    let d = OpenDispatch {
        task: "aaaa1111".into(),
        record: read_only_record("j1"),
    };
    let outcome = read_only_outcome(&[
        ("push", "read_only_discarded"),
        ("discarded_count", "12"),
        ("discarded_path_1", "a.txt"),
        ("discarded_path_2", "dir/b c.txt"),
        ("discarded_path_10", "z.txt"),
    ]);
    let note: serde_json::Value = serde_json::from_str(&collected_note(&d, &outcome)).unwrap();
    assert_eq!(note["push"], "read_only_discarded");
    assert_eq!(note["discarded_count"], "12");
    assert_eq!(
        note["discarded"],
        serde_json::json!(["a.txt", "dir/b c.txt", "z.txt"]),
        "numeric order, not text order"
    );
    assert!(note.get("discarded_path_1").is_none(), "{note}");
    let text = collected_text(&d, &outcome);
    assert!(text.contains("result: read_only_discarded 12"), "{text}");
    assert!(
        text.contains("discarded paths (3 of 12):\n  a.txt\n  dir/b c.txt\n  z.txt"),
        "{text}"
    );
}
