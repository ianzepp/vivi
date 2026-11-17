use axum::http::StatusCode;

use super::super::browser_test::Browser;
use crate::test_support::{SeedRun, Seeded, seeded};

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn finished<'a>() -> SeedRun<'a> {
    SeedRun {
        outcome: Some("finished"),
        agent_rc: Some(0),
        check_rc: Some(0),
        ..SeedRun::default()
    }
}

fn plant(store: &Seeded, job: &str, ago: i64, run: SeedRun<'_>, reason: Option<&str>) {
    store.insert(&SeedRun {
        job,
        dispatched_at: now() - ago - 600,
        collected_at: Some(now() - ago),
        ..run
    });
    store
        .conn
        .execute(
            "UPDATE slot_runs SET push = 'pushed', landing_reason = ?2 WHERE job = ?1",
            rusqlite::params![job, reason],
        )
        .unwrap();
}

fn seed(store: &Seeded) {
    let timeout = SeedRun {
        outcome: Some("timeout"),
        ..finished()
    };
    plant(store, "t1", 600, timeout, None);
    let conflict = SeedRun {
        landing: Some("conflict"),
        ..finished()
    };
    plant(
        store,
        "x1",
        900,
        conflict,
        Some("radix/<script>alert(1)</script>/a.rs\nradix/lower/b.rs"),
    );
    let red = SeedRun {
        landing: Some("red"),
        ..finished()
    };
    plant(store, "r1", 1200, red, Some("faber --test <integration>"));
    for (job, landing) in [("n1", "conflict"), ("n2", "raced"), ("n3", "red")] {
        let run = SeedRun {
            need: Some("need-again-0123456789"),
            landing: Some(landing),
            ..finished()
        };
        plant(store, job, 1500, run, None);
    }
}

#[tokio::test]
async fn the_failures_page_shows_kinds_hot_files_commands_and_repeat_needs() {
    let store = seeded(true);
    seed(&store);
    let browser = Browser::open(store.catalog()).await;
    let (status, page) = browser.get("/ui/p/demo/failures").await;
    assert_eq!(status, StatusCode::OK);
    for expected in [
        "timeout",
        "conflict",
        "landing red",
        "raced",
        // Kinds link to the filtered jobs list.
        "/ui/p/demo/jobs?failed=1&amp;kind=timeout&amp;window=24h",
        "/ui/p/demo/jobs?failed=1&amp;kind=conflict&amp;window=24h",
        // Hot files by prefix, red by command.
        "radix/lower/",
        "faber --test",
        // The repeat need links to its page.
        "/ui/p/demo/needs/need-again-0123456789",
        "need-aga",
        // Window chips keep the others reachable.
        "window=1h",
        "window=7d",
    ] {
        assert!(page.contains(expected), "{expected} missing from {page}");
    }
}

#[tokio::test]
async fn free_text_from_landings_is_escaped() {
    let store = seeded(true);
    seed(&store);
    let browser = Browser::open(store.catalog()).await;
    let (_, page) = browser.get("/ui/p/demo/failures").await;
    assert!(!page.contains("<script>alert(1)"), "{page}");
    assert!(page.contains("radix/&lt;script&gt;alert(1)&lt;"), "{page}");
    assert!(!page.contains("<integration>"));
    assert!(page.contains("&lt;integration&gt;"));
}

#[tokio::test]
async fn the_window_and_pool_come_from_the_query_string() {
    let store = seeded(true);
    seed(&store);
    let browser = Browser::open(store.catalog()).await;
    let (_, hour) = browser.get("/ui/p/demo/failures?window=1h").await;
    assert!(hour.contains("last 1h"), "{hour}");
    assert!(hour.contains("kind=timeout&amp;window=1h"));
    let (_, other_pool) = browser.get("/ui/p/demo/failures?pool=nope").await;
    assert!(other_pool.contains("No failures."), "{other_pool}");
    assert!(other_pool.contains("pool=nope") || other_pool.contains("Pool"));
    // A junk window falls back to a day.
    let (status, junk) = browser.get("/ui/p/demo/failures?window=forever").await;
    assert_eq!(status, StatusCode::OK);
    assert!(junk.contains("last 24h"));
}

#[tokio::test]
async fn a_store_without_the_slot_index_gets_the_plain_notice() {
    let store = seeded(false);
    let browser = Browser::open(store.catalog()).await;
    let (status, page) = browser.get("/ui/p/demo/failures").await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("no slot index"), "{page}");
}

#[tokio::test]
async fn the_failures_menu_entry_is_a_real_page_now() {
    let store = seeded(true);
    let browser = Browser::open(store.catalog()).await;
    let (_, page) = browser.get("/ui/p/demo/failures").await;
    assert!(!page.contains("not built yet"));
}
