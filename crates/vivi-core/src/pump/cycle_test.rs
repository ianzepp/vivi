use std::fs;

use super::{FILE_NAME, Receipts, TickRecord, read};

#[test]
fn records_running_and_finished_tick_timing() {
    let home = tempfile::tempdir().unwrap();
    let mut receipts = Receipts::default();
    receipts.begin(home.path(), "cycle-a".into(), 100).unwrap();
    let running = read(home.path()).unwrap();
    assert_eq!(running.len(), 1);
    assert!(!running[0].is_finished());
    assert_eq!(running[0].started_at, 100);

    receipts
        .finish(home.path(), "cycle-a", 105, 5, "ok")
        .unwrap();
    let done = read(home.path()).unwrap();
    assert_eq!(
        done,
        vec![TickRecord {
            id: "cycle-a".into(),
            started_at: 100,
            ended_at: Some(105),
            duration_secs: Some(5),
            outcome: Some("ok".into()),
        }]
    );
}

#[test]
fn beginning_a_tick_closes_an_orphan_as_interrupted_and_keeps_a_bound() {
    let home = tempfile::tempdir().unwrap();
    let mut receipts = Receipts::default();
    receipts.begin(home.path(), "orphan".into(), 100).unwrap();
    receipts.begin(home.path(), "next".into(), 140).unwrap();
    let records = read(home.path()).unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].outcome.as_deref(), Some("interrupted"));
    assert_eq!(records[0].duration_secs, Some(40));

    for n in 0..80 {
        let id = format!("done-{n}");
        receipts.begin(home.path(), id.clone(), 200 + n).unwrap();
        receipts.finish(home.path(), &id, 201 + n, 1, "ok").unwrap();
    }
    assert_eq!(read(home.path()).unwrap().len(), 64);
}

#[test]
fn a_tick_a_previous_process_left_open_is_closed_as_interrupted() {
    let home = tempfile::tempdir().unwrap();
    Receipts::default()
        .begin(home.path(), "crashed".into(), 100)
        .unwrap();
    let mut restarted = Receipts::default();
    restarted.begin(home.path(), "after".into(), 160).unwrap();
    let records = read(home.path()).unwrap();
    assert_eq!(records[0].id, "crashed");
    assert_eq!(records[0].outcome.as_deref(), Some("interrupted"));
    assert_eq!(records[0].duration_secs, Some(60));
    assert!(!records[1].is_finished());
}

#[test]
fn the_log_is_compacted_so_it_stays_bounded_on_disk() {
    let home = tempfile::tempdir().unwrap();
    let mut receipts = Receipts::default();
    for n in 0..1000 {
        let id = format!("t-{n}");
        receipts.begin(home.path(), id.clone(), n).unwrap();
        receipts.finish(home.path(), &id, n + 1, 1, "ok").unwrap();
    }
    let text = fs::read_to_string(home.path().join(FILE_NAME)).unwrap();
    assert!(
        text.lines().count() <= 4 * 64 + 2,
        "{} lines",
        text.lines().count()
    );
    let records = read(home.path()).unwrap();
    assert_eq!(records.len(), 64);
    assert_eq!(records.last().unwrap().id, "t-999");
    assert!(records.iter().all(TickRecord::is_finished));
}

#[test]
fn a_final_line_cut_short_by_a_write_in_progress_is_ignored() {
    let home = tempfile::tempdir().unwrap();
    let mut receipts = Receipts::default();
    receipts.begin(home.path(), "whole".into(), 100).unwrap();
    receipts.finish(home.path(), "whole", 101, 1, "ok").unwrap();
    let path = home.path().join(FILE_NAME);
    let mut text = fs::read_to_string(&path).unwrap();
    text.push_str("{\"id\":\"half");
    fs::write(&path, text).unwrap();
    let records = read(home.path()).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, "whole");
}

#[test]
fn finishing_a_tick_that_is_not_open_is_an_error() {
    let home = tempfile::tempdir().unwrap();
    let mut receipts = Receipts::default();
    receipts.begin(home.path(), "a".into(), 100).unwrap();
    assert!(receipts.finish(home.path(), "b", 101, 1, "ok").is_err());
}
