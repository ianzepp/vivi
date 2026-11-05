//! Tests for the build-wait status lines [`super::write_status`] appends.

use std::fs;
use std::sync::Arc;

use rusqlite::{Connection, OpenFlags};
use tempfile::tempdir;

use super::write_status;
use crate::pump::config::PumpConfig;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::report::{PumpRead, read};
use crate::pump::state::State;
use crate::pump::store::DB_FILENAME;
use crate::pump::store::PumpStore;
use crate::pump::store::build_waits::{BuildPrerequisite, BuildWait, PrerequisiteState};

/// The fake clock's start (1970-01-12T13:46:40Z).
const STAMP: &str = "1970-01-12T13:46:40Z";

fn context() -> (tempfile::TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    (dir, ctx, host)
}

fn wait(need: &str, record: &str, prerequisites: &[(&str, PrerequisiteState)]) -> BuildWait {
    BuildWait {
        need: need.into(),
        record: record.into(),
        prerequisites: prerequisites
            .iter()
            .map(|(handle, state)| BuildPrerequisite {
                handle: (*handle).into(),
                state: *state,
                detail: None,
                outcome: None,
            })
            .collect(),
        observed_at: 1_700_000_000,
    }
}

fn status_lines(ctx: &Ctx) -> Vec<String> {
    let text = fs::read_to_string(ctx.home.join("status.txt")).unwrap();
    text.lines().map(str::to_string).collect()
}

fn database(ctx: &Ctx) -> PumpStore {
    let store = PumpStore::open(&ctx.home.join(DB_FILENAME)).unwrap();
    store.save(&State::default()).unwrap();
    store
}

fn table_exists(path: &std::path::Path, table: &str) -> bool {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).expect("open");
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |row| row.get::<_, i64>(0),
    )
    .expect("count")
        > 0
}

#[test]
fn status_appends_one_line_per_persisted_wait_in_stable_order() {
    let (_dir, ctx, _host) = context();
    let store = database(&ctx);
    store
        .upsert_build_wait(&wait("n2", "r2", &[("d", PrerequisiteState::Open)]))
        .unwrap();
    let mut first = wait(
        "n1",
        "r1",
        &[
            ("b", PrerequisiteState::ClosedUnlanded),
            ("a", PrerequisiteState::Held),
        ],
    );
    first.prerequisites[0].outcome = Some("failed".into());
    first.prerequisites[1].detail = Some("waiting on the gate".into());
    store.upsert_build_wait(&first).unwrap();

    write_status(&ctx, &State::default());
    let lines = status_lines(&ctx);
    assert_eq!(lines[0], STAMP);
    assert_eq!(
        lines[1..],
        [
            String::from(
                "need n1 waiting on a held (waiting on the gate), \
                 b closed_unlanded outcome failed"
            ),
            String::from("need n2 waiting on d open"),
        ]
    );
    // Two writes of the same store agree.
    write_status(&ctx, &State::default());
    assert_eq!(status_lines(&ctx), lines);
}

#[test]
fn status_lines_carry_the_same_handles_and_states_as_the_report() {
    let (_dir, ctx, _host) = context();
    let store = database(&ctx);
    store
        .upsert_build_wait(&wait("n1", "r1", &[("a", PrerequisiteState::Held)]))
        .unwrap();
    write_status(&ctx, &State::default());
    let lines = status_lines(&ctx);
    let PumpRead::Ready(snapshot) = read(&ctx.home).unwrap() else {
        panic!("expected a ready snapshot");
    };
    assert_eq!(snapshot.build_waits().len(), lines.len() - 1);
    for (line, persisted) in lines[1..].iter().zip(snapshot.build_waits()) {
        assert!(
            line.starts_with(&format!("need {} waiting on", persisted.need)),
            "{line}"
        );
        for prerequisite in &persisted.prerequisites {
            assert!(line.contains(&prerequisite.handle), "{line}");
            assert!(line.contains("held"), "{line}");
        }
    }
}

#[test]
fn status_keeps_the_stamp_and_cadence_lines_before_the_wait_lines() {
    let (_dir, ctx, _host) = context();
    let store = database(&ctx);
    store
        .upsert_build_wait(&wait("n1", "r1", &[("a", PrerequisiteState::Held)]))
        .unwrap();
    let mut st = State::default();
    let mut cadence = crate::pump::cadence::CadenceState::default();
    cadence.scans.insert(
        String::from("digest"),
        crate::pump::cadence::RoleScan {
            name: String::from("digest"),
            kind: Some(String::from("digest")),
            status: String::from("active"),
            cadence: Some(String::from("1h")),
            pool: None,
            open_wants: 0,
            open_needs: 0,
            has_charter: true,
            schedule_state: String::from("ready"),
            last_signal_at: None,
            next_due_at: None,
        },
    );
    crate::pump::cadence::set_state(&mut st, &cadence);
    write_status(&ctx, &st);
    let lines = status_lines(&ctx);
    assert_eq!(lines[0], STAMP);
    assert!(lines[1].starts_with("cadence digest "), "{:?}", lines);
    assert_eq!(lines[2], "need n1 waiting on a held");
}

#[test]
fn a_status_write_without_a_database_writes_only_the_stamp_and_creates_none() {
    let (_dir, ctx, host) = context();
    write_status(&ctx, &State::default());
    assert_eq!(status_lines(&ctx), [String::from(STAMP)]);
    assert!(!ctx.home.join(DB_FILENAME).exists());
    assert!(host.logs().is_empty());
}

#[test]
fn an_unreadable_store_is_logged_and_no_wait_line_is_fabricated() {
    let (_dir, ctx, host) = context();
    fs::create_dir_all(&ctx.home).unwrap();
    fs::write(ctx.home.join(DB_FILENAME), b"not sqlite").unwrap();
    write_status(&ctx, &State::default());
    assert_eq!(status_lines(&ctx), [String::from(STAMP)]);
    assert!(
        host.logs()
            .iter()
            .any(|line| line.starts_with("status waits unreadable: ")),
        "{:?}",
        host.logs()
    );
}

#[test]
fn a_wait_line_disappears_after_the_wait_is_removed() {
    let (_dir, ctx, _host) = context();
    let store = database(&ctx);
    store
        .upsert_build_wait(&wait("n1", "r1", &[("a", PrerequisiteState::Held)]))
        .unwrap();
    write_status(&ctx, &State::default());
    assert_eq!(status_lines(&ctx).len(), 2);
    store.remove_build_wait("n1").unwrap();
    write_status(&ctx, &State::default());
    assert_eq!(status_lines(&ctx), [String::from(STAMP)]);
}

#[test]
fn a_database_without_the_wait_table_renders_no_wait_line_and_keeps_the_table_out() {
    let (_dir, ctx, _host) = context();
    drop(database(&ctx));
    let path = ctx.home.join(DB_FILENAME);
    Connection::open(&path)
        .unwrap()
        .execute_batch("DROP TABLE build_waits")
        .unwrap();
    write_status(&ctx, &State::default());
    assert_eq!(status_lines(&ctx), [String::from(STAMP)]);
    assert!(!table_exists(&path, "build_waits"));
}

#[test]
fn a_status_write_leaves_the_waits_and_the_exceptions_untouched() {
    let (_dir, ctx, _host) = context();
    let store = database(&ctx);
    let recorded = wait("n1", "r1", &[("a", PrerequisiteState::Held)]);
    store.upsert_build_wait(&recorded).unwrap();
    write_status(&ctx, &State::default());
    assert_eq!(store.build_waits().unwrap(), vec![recorded]);
    assert!(store.exceptions().unwrap().is_empty());
    assert!(!ctx.home.join("exceptions.txt").exists());
}
