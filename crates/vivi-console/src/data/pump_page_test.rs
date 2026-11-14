use rusqlite::{Connection, params};
use vivi_core::pump::store::PumpStore;

use super::*;
use crate::data::pump_db::pump_db_path;

fn project(config: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".vivi")).unwrap();
    std::fs::write(dir.path().join(".vivi/mailspace.toml"), config).unwrap();
    dir
}

/// A pump project with its store created and a writer left open, as a live
/// pump does.
pub(crate) fn pump_project() -> (tempfile::TempDir, Connection) {
    let dir = project("name = \"demo\"\n\n[pump]\n");
    let path = pump_db_path(dir.path()).unwrap().unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    drop(PumpStore::open(&path).unwrap());
    let writer = Connection::open(&path).unwrap();
    writer
        .query_row("SELECT count(*) FROM units", [], |row| row.get::<_, i64>(0))
        .unwrap();
    (dir, writer)
}

pub(crate) fn unit(conn: &Connection, id: &str, status: &str) {
    conn.execute(
        "INSERT INTO units (id, need, title, brief, write_scope_json, after_json, \
         proof_json, tools, status, attempts, since, outside_json, replaced_by_json, \
         kind_reported, extra_json) \
         VALUES (?1, 'need-1', 't', 'b', '[]', '[]', '[]', '', ?2, 1, 0, '[]', '[]', 0, '{}')",
        params![id, status],
    )
    .unwrap();
}

pub(crate) fn event(conn: &Connection, unit: &str, attempt: i64, why: &str, at: i64) {
    conn.execute(
        "INSERT INTO unit_events (unit, attempt, model, why, detail, at) \
         VALUES (?1, ?2, 'model-a', ?3, 'detail text', ?4)",
        params![unit, attempt, why, at],
    )
    .unwrap();
}

fn data(root: &std::path::Path) -> PumpData {
    match load(root, 10_000).unwrap().store {
        StoreState::Ready(data) => *data,
        other => panic!("expected a readable store, got {other:?}"),
    }
}

#[test]
fn a_project_without_a_pump_says_so() {
    let dir = project("name = \"demo\"\n");
    let page = load(dir.path(), 0).unwrap();
    assert_eq!(page.store, StoreState::NoPump);
    assert_eq!(page.health, PumpView::NoPump);
}

#[test]
fn a_configured_pump_with_no_store_is_unavailable_not_an_error() {
    let dir = project("name = \"demo\"\n\n[pump]\n");
    let page = load(dir.path(), 0).unwrap();
    let StoreState::Unavailable(message) = page.store else {
        panic!("expected unavailable");
    };
    assert!(message.contains("no store"), "{message}");
}

#[test]
fn units_are_counted_by_status() {
    let (dir, _writer) = pump_project();
    let conn = Connection::open(pump_db_path(dir.path()).unwrap().unwrap()).unwrap();
    for (id, status) in [("u1", "merged"), ("u2", "merged"), ("u3", "inflight")] {
        unit(&conn, id, status);
    }
    let data = data(dir.path());
    assert_eq!(
        data.units,
        [("inflight".to_string(), 1), ("merged".to_string(), 2)]
    );
}

#[test]
fn unit_events_are_newest_first_and_capped_at_fifty() {
    let (dir, _writer) = pump_project();
    let conn = Connection::open(pump_db_path(dir.path()).unwrap().unwrap()).unwrap();
    for n in 0..60 {
        event(&conn, "u1", n, "check-red", 100 + n);
    }
    let data = data(dir.path());
    assert_eq!(data.events.len(), 50);
    assert_eq!(data.events[0].attempt, 59);
    assert_eq!(data.events[0].why, "check-red");
    assert_eq!(data.events[0].model.as_deref(), Some("model-a"));
    assert_eq!(data.events[49].attempt, 10);
}

#[test]
fn gates_carry_their_failures_and_exceptions_come_newest_first() {
    let (dir, _writer) = pump_project();
    let conn = Connection::open(pump_db_path(dir.path()).unwrap().unwrap()).unwrap();
    conn.execute_batch(
        "INSERT INTO gates (tip, at, merges) VALUES ('tip-one', 10, 2);
         INSERT INTO gates (tip, at, merges) VALUES ('tip-two', 20, 3);
         INSERT INTO gate_failures (gate_id, name, kind) VALUES (2, 'integration', 'test');
         INSERT INTO gate_failures (gate_id, name, kind) VALUES (2, 'lint', 'check');
         INSERT INTO exceptions (at, kind, unit, text) VALUES (5, 'stuck', 'u1', 'first');
         INSERT INTO exceptions (at, kind, unit, text) VALUES (6, 'stuck', 'u2', 'second');",
    )
    .unwrap();
    let data = data(dir.path());
    let tips: Vec<&str> = data.gates.iter().map(|gate| gate.tip.as_str()).collect();
    assert_eq!(tips, ["tip-two", "tip-one"]);
    assert_eq!(
        data.gates[0].failures,
        [
            ("integration".to_string(), "test".to_string()),
            ("lint".to_string(), "check".to_string())
        ]
    );
    assert!(data.gates[1].failures.is_empty());
    let texts: Vec<&str> = data.exceptions.iter().map(|e| e.text.as_str()).collect();
    assert_eq!(texts, ["second", "first"]);
}

#[test]
fn rung_runs_come_from_the_newest_flow_steps_with_their_duration_and_rc() {
    let (dir, _writer) = pump_project();
    let conn = Connection::open(pump_db_path(dir.path()).unwrap().unwrap()).unwrap();
    conn.execute_batch(
        "INSERT INTO flow_steps (need, run, node, task, state) VALUES ('n', 1, 'build', 'task-a', 'done');
         INSERT INTO flow_steps (need, run, node, task, state) VALUES ('n', 1, 'build', 'task-b', 'done');
         INSERT INTO rung_runs VALUES ('task-a', 'fmt', 100, 130, 0);
         INSERT INTO rung_runs VALUES ('task-b', 'test', 200, 290, 101);
         INSERT INTO rung_runs VALUES ('task-b', 'lint', 300, NULL, NULL);
         INSERT INTO rung_runs VALUES ('task-old', 'fmt', 1, 2, 0);",
    )
    .unwrap();
    let data = data(dir.path());
    let seen: Vec<(&str, &str, Option<i64>)> = data
        .rungs
        .iter()
        .map(|run| (run.task.as_str(), run.rung.as_str(), run.rc))
        .collect();
    assert_eq!(
        seen,
        [
            ("task-b", "lint", None),
            ("task-b", "test", Some(101)),
            ("task-a", "fmt", Some(0)),
        ],
        "a task with no recent flow step is not read"
    );
    assert_eq!(data.rungs[1].finished_at, Some(290));
}

#[test]
fn the_control_mode_is_read() {
    let (dir, _writer) = pump_project();
    let conn = Connection::open(pump_db_path(dir.path()).unwrap().unwrap()).unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO pump_control (singleton, mode, shutdown_requested) \
         VALUES (1, 'paused', 1)",
        [],
    )
    .unwrap();
    assert_eq!(data(dir.path()).control, Some(("paused".to_string(), true)));
}

#[test]
fn reading_the_page_changes_nothing_on_disk() {
    let (dir, _writer) = pump_project();
    let path = pump_db_path(dir.path()).unwrap().unwrap();
    let before = std::fs::read(&path).unwrap();
    let _ = data(dir.path());
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
