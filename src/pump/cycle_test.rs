use super::{TickRecord, begin, finish, read};

#[test]
fn records_running_and_finished_tick_timing() {
    let home = tempfile::tempdir().unwrap();
    begin(home.path(), "cycle-a".into(), 100).unwrap();
    let running = read(home.path()).unwrap();
    assert_eq!(running.len(), 1);
    assert!(!running[0].is_finished());
    assert_eq!(running[0].started_at, 100);

    finish(home.path(), "cycle-a", 105, 5, "ok").unwrap();
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
    begin(home.path(), "orphan".into(), 100).unwrap();
    begin(home.path(), "next".into(), 140).unwrap();
    let records = read(home.path()).unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].outcome.as_deref(), Some("interrupted"));
    assert_eq!(records[0].duration_secs, Some(40));

    for n in 0..80 {
        let id = format!("done-{n}");
        begin(home.path(), id.clone(), 200 + n).unwrap();
        finish(home.path(), &id, 201 + n, 1, "ok").unwrap();
    }
    assert_eq!(read(home.path()).unwrap().len(), 64);
}
