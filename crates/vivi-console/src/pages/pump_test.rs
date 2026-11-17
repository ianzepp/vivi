use axum::http::StatusCode;

use super::super::browser_test::Browser;
use crate::data::pump_db::pump_db_path;
use crate::data::pump_page::tests::{event, pump_project, unit};
use crate::host::{Catalog, ProjectRef};
use crate::test_support::seeded;

fn catalog_of(root: &std::path::Path) -> Catalog {
    Catalog {
        projects: vec![ProjectRef {
            id: "proj-1".into(),
            name: "demo".into(),
            mailspace_root: root.to_path_buf(),
        }],
    }
}

#[tokio::test]
async fn a_project_with_no_pump_gets_a_plain_message() {
    let store = seeded(true);
    let browser = Browser::open(store.catalog()).await;
    let (status, page) = browser.get("/ui/p/demo/pump").await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("has no pump"), "{page}");
    assert!(!page.contains("not built yet"));
}

#[tokio::test]
async fn a_configured_pump_with_no_store_says_it_cannot_be_shown() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".vivi")).unwrap();
    std::fs::write(
        dir.path().join(".vivi/mailspace.toml"),
        "name = \"demo\"\n\n[pump]\n",
    )
    .unwrap();
    let browser = Browser::open(catalog_of(dir.path())).await;
    let (status, page) = browser.get("/ui/p/demo/pump").await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("The pump store cannot be shown"), "{page}");
}

#[tokio::test]
async fn the_pump_page_shows_units_events_gates_and_exceptions() {
    let (dir, _writer) = pump_project();
    let conn = rusqlite::Connection::open(pump_db_path(dir.path()).unwrap().unwrap()).unwrap();
    unit(&conn, "unit-merged-1", "merged");
    unit(&conn, "unit-stuck-1", "stuck");
    event(&conn, "unit-stuck-1", 3, "check-red", 4_000_000_000);
    conn.execute(
        "UPDATE unit_events SET detail = '<img src=x onerror=alert(1)>'",
        [],
    )
    .unwrap();
    conn.execute_batch(
        "INSERT INTO gates (tip, at, merges) VALUES ('tipabcdef123', 10, 2);
         INSERT INTO gate_failures (gate_id, name, kind) VALUES (1, 'integration', 'test');
         INSERT INTO exceptions (at, kind, unit, text) VALUES (5, 'stuck', 'unit-stuck-1', 'it stuck');
         INSERT INTO pump_control (singleton, mode, shutdown_requested) VALUES (1, 'running', 0)
           ON CONFLICT(singleton) DO UPDATE SET mode = 'running';",
    )
    .unwrap();
    let browser = Browser::open(catalog_of(dir.path())).await;
    let (status, page) = browser.get("/ui/p/demo/pump").await;
    assert_eq!(status, StatusCode::OK);
    for expected in [
        "Units by status",
        "merged",
        "stuck",
        "check-red",
        "integration (test)",
        "tipabcde",
        "it stuck",
        "running",
    ] {
        assert!(page.contains(expected), "{expected} missing from {page}");
    }
    assert!(!page.contains("<img src=x"), "event detail must be escaped");
    assert!(page.contains("&lt;img src=x onerror=alert(1)&gt;"));
}
