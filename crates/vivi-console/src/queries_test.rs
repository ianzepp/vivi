use rusqlite::types::Value;
use vivi_core::pump::flow::index::StepState;

use super::*;
use crate::data::needs::seed::{PumpSeed, seed_pump};
use crate::test_support::{SeedRun, seeded};

/// Plan fragments that mean a full table scan of a big table.
const FORBIDDEN: &[&str] = &[
    "SCAN slot_runs",
    "SCAN mailspace_events",
    "SCAN messages",
    "SCAN message_metadata",
    "SCAN mailspace_links",
    "SCAN blobs",
];

/// The same for `pump.db`: none of these tables may be scanned.
const FORBIDDEN_PUMP: &[&str] = &[
    "SCAN units",
    "SCAN unit_events",
    "SCAN gates",
    "SCAN gate_failures",
    "SCAN exceptions",
    "SCAN flow_steps",
    "SCAN rung_runs",
];

fn bind(sample: &Sample) -> Value {
    match sample {
        Sample::Int(value) => Value::Integer(*value),
        // `seeded_store` rows span 1_000..13_000.
        Sample::Recent => Value::Integer(12_000),
        Sample::Text(text) => Value::Text((*text).to_string()),
    }
}

fn plan(conn: &rusqlite::Connection, query: &Query) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!("EXPLAIN QUERY PLAN {}", query.sql))
        .unwrap_or_else(|e| panic!("query {} does not prepare: {e}", query.name));
    let params: Vec<Value> = query.samples.iter().map(bind).collect();
    assert_eq!(
        params.len(),
        stmt.parameter_count(),
        "query {} needs one sample per parameter",
        query.name
    );
    let rows = stmt
        .query_map(rusqlite::params_from_iter(params), |row| {
            row.get::<_, String>(3)
        })
        .unwrap();
    rows.map(Result::unwrap).collect()
}

/// A store with enough rows that the planner has a reason to pick indexes.
fn seeded_store(analyze: bool) -> crate::test_support::Seeded {
    let store = seeded(true);
    for n in 0..200 {
        store.insert(&SeedRun {
            job: &format!("job-{n}"),
            slot: &format!("slot-{}", n % 8),
            dispatched_at: 1_000 + n * 60,
            collected_at: (n % 5 != 0).then_some(1_000 + n * 60 + 600),
            outcome: (n % 5 != 0).then_some("finished"),
            agent_rc: (n % 5 != 0).then_some(0),
            ..SeedRun::default()
        });
    }
    if analyze {
        store.conn.execute_batch("ANALYZE").unwrap();
    }
    store
}

#[test]
fn no_console_query_scans_a_big_table() {
    // Production stores carry no statistics; an analyzed one must agree.
    for analyze in [false, true] {
        check_plans(&seeded_store(analyze), analyze);
    }
}

fn check_plans(store: &crate::test_support::Seeded, analyze: bool) {
    let mut failures = Vec::new();
    for query in REGISTRY {
        let lines = plan(&store.conn, query);
        for line in &lines {
            if FORBIDDEN.iter().any(|bad| line.contains(bad)) {
                failures.push(format!("{} (analyze={analyze}): {line}", query.name));
            }
        }
    }
    assert!(failures.is_empty(), "scans found:\n{}", failures.join("\n"));
}

/// A pump store with enough rows that the planner has a reason to use its
/// indexes, held open by a plain connection as a live pump would.
fn pump_conn(analyze: bool) -> (tempfile::TempDir, rusqlite::Connection) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(vivi_core::pump::store::DB_FILENAME);
    drop(vivi_core::pump::store::PumpStore::open(&path).unwrap());
    let conn = rusqlite::Connection::open(&path).unwrap();
    for n in 0..200 {
        conn.execute(
            "INSERT INTO units (id, need, title, brief, write_scope_json, after_json, \
             proof_json, tools, status, attempts, since, outside_json, replaced_by_json, \
             kind_reported, extra_json) \
             VALUES (?1, 'n', 't', 'b', '[]', '[]', '[]', '', ?2, 1, 0, '[]', '[]', 0, '{}')",
            rusqlite::params![format!("u{n}"), ["merged", "pending", "stuck"][n % 3]],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO unit_events (unit, attempt, why, detail, at) VALUES (?1, 1, 'w', 'd', ?2)",
            rusqlite::params![format!("u{n}"), n as i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO gates (tip, at, merges) VALUES ('t', ?1, 1)",
            [n as i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO gate_failures (gate_id, name, kind) VALUES (?1, 'g', 'k')",
            [n as i64 + 1],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO exceptions (at, kind, unit, text) VALUES (?1, 'k', 'u', 't')",
            [n as i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO flow_steps (need, run, node, task, state) VALUES ('n', 1, 'b', ?1, 's')",
            [format!("task{n}")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO rung_runs (task, rung, started_at) VALUES (?1, 'fmt', ?2)",
            rusqlite::params![format!("task{n}"), n as i64],
        )
        .unwrap();
    }
    if analyze {
        conn.execute_batch("ANALYZE").unwrap();
    }
    (dir, conn)
}

#[test]
fn pump_plans_never_scan() {
    let mut failures = Vec::new();
    for analyze in [false, true] {
        let (_dir, conn) = pump_conn(analyze);
        for query in PUMP_REGISTRY {
            for line in plan(&conn, query) {
                if FORBIDDEN_PUMP.iter().any(|bad| line.contains(bad)) {
                    failures.push(format!("{} (analyze={analyze}): {line}", query.name));
                }
            }
        }
    }
    assert!(failures.is_empty(), "scans found:\n{}", failures.join("\n"));
}

#[test]
fn pump_registry_names_are_unique_and_apart_from_the_mailspace_ones() {
    let mut names: Vec<&str> = PUMP_REGISTRY
        .iter()
        .chain(REGISTRY.iter())
        .map(|query| query.name)
        .collect();
    names.sort_unstable();
    let before = names.len();
    names.dedup();
    assert_eq!(before, names.len());
}

#[test]
fn the_plan_check_would_catch_a_scan() {
    let store = seeded_store(false);
    let scan = Query {
        name: "scan",
        sql: "SELECT COUNT(*) FROM slot_runs WHERE machine = ?1",
        samples: &[Sample::Text("m")],
    };
    let lines = plan(&store.conn, &scan);
    assert!(
        lines.iter().any(|line| line.contains("SCAN slot_runs")),
        "{lines:?}"
    );
}

#[test]
fn registry_names_are_unique() {
    let mut names: Vec<&str> = REGISTRY.iter().map(|query| query.name).collect();
    names.sort_unstable();
    let before = names.len();
    names.dedup();
    assert_eq!(before, names.len());
}

/// Pages never carry SQL: every statement lives in `queries.rs`.
#[test]
fn no_sql_text_outside_the_registry() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut hits = Vec::new();
    scan(&src, &mut hits);
    assert!(hits.is_empty(), "SQL outside queries.rs: {hits:?}");
}

fn scan(dir: &std::path::Path, hits: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan(&path, hits);
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let exempt =
            name == "queries.rs" || name.ends_with("_test.rs") || name == "test_support.rs";
        if exempt || path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for word in ["SELECT ", "INSERT ", "UPDATE ", "DELETE FROM", "PRAGMA "] {
            if text.contains(word) {
                hits.push(format!("{name}: {word}"));
            }
        }
    }
}

/// A windowed failures query must walk the time index, not another index
/// that happens to avoid a SCAN (the need index covers every run of a need).
#[test]
fn the_failures_queries_walk_the_collected_at_index() {
    for analyze in [false, true] {
        let store = seeded_store(analyze);
        for query in [&FAILURE_KINDS, &LANDING_REASONS, &NEED_KINDS] {
            let lines = plan(&store.conn, query);
            assert!(
                lines
                    .iter()
                    .any(|line| line.contains("slot_runs_collected_idx")),
                "{} (analyze={analyze}): {lines:?}",
                query.name
            );
        }
    }
}
/// Plan fragments that mean a full scan of a table the needs pages read.
const NEEDS_FORBIDDEN: &[&str] = &[
    "SCAN work_graph_nodes",
    "SCAN work_graphs",
    "SCAN mailspace_links",
    "SCAN message_metadata",
    "SCAN blobs",
    // The aliases the needs queries give those tables.
    "SCAN n",
    "SCAN md",
    "SCAN b",
    "SCAN l",
];

/// True when the plan line scans exactly the named table or alias (a
/// `SCAN md USING ...` is one; `SCAN mailspace_links` is not `SCAN md`).
fn scans(line: &str, bad: &str) -> bool {
    line == bad || line.starts_with(&format!("{bad} "))
}

#[test]
fn the_needs_queries_do_not_scan_their_tables() {
    for analyze in [false, true] {
        let store = seeded_store(analyze);
        let mut failures = Vec::new();
        for query in REGISTRY {
            for line in plan(&store.conn, query) {
                if NEEDS_FORBIDDEN.iter().any(|bad| scans(&line, bad)) {
                    failures.push(format!("{} (analyze={analyze}): {line}", query.name));
                }
            }
        }
        assert!(failures.is_empty(), "scans found:\n{}", failures.join("\n"));
    }
}

/// Plan fragments that mean a full scan of a pump store table.
const PUMP_FORBIDDEN: &[&str] = &[
    "SCAN flow_runs",
    "SCAN flow_steps",
    "SCAN step_jobs",
    "SCAN units",
    "SCAN unit_events",
    "SCAN slice_scope",
    "SCAN slice_links",
    "SCAN rung_runs",
    "SCAN plan_revisions",
    "SCAN need_branches",
    "SCAN fanout_open",
    "SCAN fanout_starts",
    "SCAN restart_entries",
    // The aliases the slice query gives flow_steps, slice_scope and slice_links.
    "SCAN s",
    "SCAN sc",
    "SCAN l",
];

/// A pump store with enough flow rows that the planner has a reason to pick
/// indexes. The writer stays open, as a running pump's does.
fn seeded_pump(analyze: bool) -> (crate::test_support::Seeded, PumpSeed) {
    let store = seeded(true);
    let pump = seed_pump(&store);
    for n in 0..40_u32 {
        let need = format!("{n:08x}");
        pump.run(&need, 1, "build", "plan", "live");
        pump.step(&need, 1, "sentinel", &format!("s-{n}"), StepState::Running);
        pump.step(&need, 1, "build1", &format!("b-{n}"), StepState::Running);
        pump.slice(&need, 1, &format!("x{n}"), &format!("b-{n}"), &["a.rs"]);
        pump.job(
            &need,
            1,
            "build1",
            &format!("b-{n}"),
            &format!("job-{n}"),
            1,
        );
        pump.rung(&format!("b-{n}"), "task", 100, Some(200), Some(0));
        pump.plan(&need, 1, "planned", "ok", 100);
        pump.branch(&need, &format!("vivi/need/{need}"));
        pump.store.set_restart_entry(&need, 1, "plan").unwrap();
        pump.store
            .set_open_fanout(&need, 1, &format!("b-{n}"))
            .unwrap();
        pump.store
            .set_fanout_start(&format!("b-{n}"), Some("abc"))
            .unwrap();
    }
    if analyze {
        pump.writer.execute_batch("ANALYZE").unwrap();
    }
    (store, pump)
}

#[test]
fn no_pump_query_scans_a_pump_table() {
    for analyze in [false, true] {
        let (_store, pump) = seeded_pump(analyze);
        let mut failures = Vec::new();
        for query in PUMP_REGISTRY {
            let lines = plan(&pump.writer, query);
            assert!(!lines.is_empty(), "{} has no plan", query.name);
            for line in &lines {
                if PUMP_FORBIDDEN.iter().any(|bad| scans(line, bad)) {
                    failures.push(format!("{} (analyze={analyze}): {line}", query.name));
                }
            }
        }
        assert!(failures.is_empty(), "scans found:\n{}", failures.join("\n"));
    }
}

#[test]
fn the_pump_plan_check_would_catch_a_scan() {
    let (_store, pump) = seeded_pump(false);
    let scan = Query {
        name: "scan",
        sql: "SELECT COUNT(*) FROM flow_steps WHERE node = ?1",
        samples: &[Sample::Text("build1")],
    };
    let lines = plan(&pump.writer, &scan);
    assert!(
        lines.iter().any(|line| line.contains("SCAN flow_steps")),
        "{lines:?}"
    );
}

#[test]
fn pump_and_mailspace_query_names_do_not_collide() {
    let mut names: Vec<&str> = PUMP_REGISTRY
        .iter()
        .chain(REGISTRY.iter())
        .map(|query| query.name)
        .collect();
    names.sort_unstable();
    let before = names.len();
    names.dedup();
    assert_eq!(before, names.len());
}
