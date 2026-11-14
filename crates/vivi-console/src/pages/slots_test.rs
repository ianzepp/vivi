use axum::http::StatusCode;

use crate::pages::test_support::get;
use crate::test_support::{SeedRun, seeded};

/// A pool that names its slots, and a pool that only counts them.
const CONFIG: &str = r#"name = "demo"

[pools.studio]
provider = "ssh"

[pools.counted]
provider = "local"
slots = 3

[[slot]]
name = "radix-1"
repository = "owner/repo"
host = "studio-01"
dir = "/work/radix-1"
pools = ["studio"]

[[slot]]
name = "radix-2"
repository = "owner/repo"
host = "studio-01"
dir = "/work/radix-2"
pools = ["studio"]
"#;

fn project() -> crate::test_support::Seeded {
    let store = seeded(true);
    std::fs::write(store.root.join(".vivi/mailspace.toml"), CONFIG).unwrap();
    store.insert(&SeedRun {
        job: "job-1",
        pool: "studio",
        slot: "radix-1",
        machine: "studio-01",
        dispatched_at: chrono::Utc::now().timestamp() - 300,
        ..SeedRun::default()
    });
    store
}

#[tokio::test]
async fn the_slots_page_lists_every_declared_slot_by_name() {
    let store = project();
    let (status, body) = get(&store.root, "/ui/p/demo/slots").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("slots/studio/radix-1"), "busy and named");
    assert!(
        body.contains("slots/studio/radix-2"),
        "declared, never ran, still named: {body}"
    );
    assert!(body.contains("no runs recorded"));
    assert!(body.contains("tile tile-busy") && body.contains("tile tile-free"));
    assert!(
        body.contains("3 declared slot(s) in pool counted"),
        "a counting pool stays a count"
    );
    assert!(!body.contains("declared slot(s) in pool studio"));
}

#[tokio::test]
async fn the_overview_names_the_slots_of_each_pool_and_counts_them() {
    let store = project();
    let (status, body) = get(&store.root, "/ui/p/demo/overview").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("slots/studio/radix-2"), "free named slot");
    assert!(body.contains("slots/studio/radix-1"), "busy named slot");
    assert!(body.contains("<th>Slots</th>"));
    assert!(body.contains("1 busy / 5"), "2 named + 3 counted: {body}");
}
