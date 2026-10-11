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
