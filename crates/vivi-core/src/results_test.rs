use super::*;

fn run(id: &str, suite: &str, commit: &str) -> Run {
    Run {
        id: id.to_owned(),
        project: String::from("project-a"),
        role: String::from("check"),
        target: String::from("default"),
        suite: suite.to_owned(),
        commit: commit.to_owned(),
        started_at: 100,
        finished_at: None,
        host: String::from("host-a"),
        job: Some(String::from("job-1")),
        task: Some(String::from("task-1")),
    }
}

fn row(case: &str, status: &str) -> ResultRow {
    ResultRow {
        case: case.to_owned(),
        status: status.to_owned(),
        phase: None,
        diagnostic: None,
        millis: None,
    }
}

#[test]
fn a_fresh_database_gets_the_schema_and_a_reopen_keeps_the_version() {
    let dir = tempfile::tempdir().unwrap();
    let path = db_path(dir.path());
    {
        let store = ResultsStore::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    }
    {
        let store = ResultsStore::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    }
    let conn = Connection::open(&path).unwrap();
    let runs: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'runs'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'rows'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!((runs, rows), (1, 1));
}

#[test]
fn an_older_user_version_is_migrated_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", 0).unwrap();
    drop(conn);
    let store = ResultsStore::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
}

#[test]
fn a_newer_schema_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(DB_FILENAME);
    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
        .unwrap();
    drop(conn);
    let error = ResultsStore::open(&path).err().unwrap().to_string();
    assert!(error.contains("newer than this binary"), "{error}");
}

#[test]
fn insert_then_read_the_run_and_its_rows() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = ResultsStore::open_project(dir.path()).unwrap();
    store
        .open_run(&run("run-1", "unit", "a".repeat(40).as_str()))
        .unwrap();
    let rows = vec![
        ResultRow {
            case: String::from("beta"),
            status: String::from("fail"),
            phase: Some(String::from("fast")),
            diagnostic: Some(String::from("boom\nsecond line")),
            millis: Some(12),
        },
        row("alpha", "pass"),
    ];
    assert_eq!(store.insert_rows("run-1", &rows).unwrap(), 2);
    let stored = store.rows("run-1").unwrap();
    assert_eq!(stored.len(), 2);
    assert_eq!(stored[0].case, "alpha");
    assert_eq!(stored[1].case, "beta");
    assert_eq!(stored[1].diagnostic.as_deref(), Some("boom"));
    assert_eq!(stored[1].phase.as_deref(), Some("fast"));
    assert_eq!(stored[1].millis, Some(12));
    let run = store.run("run-1").unwrap().unwrap();
    assert_eq!(run.suite, "unit");
    assert_eq!(run.finished_at, None);
}

#[test]
fn runs_are_found_by_project_target_suite_and_full_commit() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = ResultsStore::open_project(dir.path()).unwrap();
    let commit = "a".repeat(40);
    let mut first = run("run-1", "unit", &commit);
    first.started_at = 10;
    let mut second = run("run-2", "unit", &commit);
    second.started_at = 20;
    store.open_run(&first).unwrap();
    store.open_run(&second).unwrap();
    store.open_run(&run("run-3", "other", &commit)).unwrap();
    let found = store
        .runs_for_commit("project-a", "default", "unit", &commit)
        .unwrap();
    assert_eq!(
        found.iter().map(|run| run.id.as_str()).collect::<Vec<_>>(),
        ["run-2", "run-1"]
    );
    assert!(
        store
            .runs_for_commit("project-a", "default", "unit", &commit[..12])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn the_indexes_for_the_commit_lookup_and_a_run_are_present() {
    let dir = tempfile::tempdir().unwrap();
    let path = db_path(dir.path());
    drop(ResultsStore::open(&path).unwrap());
    let conn = Connection::open(&path).unwrap();
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'index'")
        .unwrap();
    let names: Vec<String> = stmt
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(names.iter().any(|name| name == "runs_by_commit_idx"));
    assert!(names.iter().any(|name| name == "rows_by_run_idx"));
}

#[test]
fn a_case_submitted_twice_in_one_run_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = ResultsStore::open_project(dir.path()).unwrap();
    store
        .open_run(&run("run-1", "unit", &"a".repeat(40)))
        .unwrap();
    store.insert_rows("run-1", &[row("same", "pass")]).unwrap();
    let again = store
        .insert_rows("run-1", &[row("same", "fail")])
        .unwrap_err();
    assert!(again.to_string().contains("submitted twice"), "{again}");
    let batch = store
        .insert_rows("run-1", &[row("new", "pass"), row("new", "fail")])
        .unwrap_err();
    assert!(batch.to_string().contains("submitted twice"), "{batch}");
}

#[test]
fn a_closed_run_refuses_rows_and_keeps_its_first_close() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = ResultsStore::open_project(dir.path()).unwrap();
    store
        .open_run(&run("run-1", "unit", &"a".repeat(40)))
        .unwrap();
    assert!(store.close_run("run-1", 50).unwrap());
    assert!(store.close_run("run-1", 60).unwrap());
    assert_eq!(store.run("run-1").unwrap().unwrap().finished_at, Some(50));
    let closed = store
        .insert_rows("run-1", &[row("case", "pass")])
        .unwrap_err();
    assert!(closed.to_string().contains("closed"), "{closed}");
}

#[test]
fn validation_rejects_unknown_status_empty_and_oversized_fields() {
    let empty = validate_row(row("  ", "pass")).unwrap_err();
    assert!(empty.contains("empty"), "{empty}");
    let unknown = validate_row(row("case", "perhaps")).unwrap_err();
    assert!(unknown.contains("unknown status"), "{unknown}");
    let long_case = validate_row(row(&"x".repeat(MAX_CASE_CHARS + 1), "pass")).unwrap_err();
    assert!(long_case.contains("case is longer"), "{long_case}");
    let long_phase = validate_row(ResultRow {
        phase: Some("p".repeat(MAX_PHASE_CHARS + 1)),
        ..row("case", "pass")
    })
    .unwrap_err();
    assert!(long_phase.contains("phase is longer"), "{long_phase}");
    let long_diagnostic = validate_row(ResultRow {
        diagnostic: Some("d".repeat(MAX_DIAGNOSTIC_CHARS + 1)),
        ..row("case", "pass")
    })
    .unwrap_err();
    assert!(
        long_diagnostic.contains("diagnostic is longer"),
        "{long_diagnostic}"
    );
    let normalized = validate_row(ResultRow {
        phase: Some(String::from("  ")),
        diagnostic: Some(String::from("first\nsecond")),
        ..row(" case ", "pass")
    })
    .unwrap();
    assert_eq!(normalized.case, "case");
    assert_eq!(normalized.phase, None);
    assert_eq!(normalized.diagnostic.as_deref(), Some("first"));
}

#[test]
fn an_unknown_run_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = ResultsStore::open_project(dir.path()).unwrap();
    let unknown = store
        .insert_rows("missing", &[row("case", "pass")])
        .unwrap_err();
    assert!(unknown.to_string().contains("unknown run"), "{unknown}");
    assert!(!store.close_run("missing", 1).unwrap());
}
