use axum::http::StatusCode;
use vivi_core::pump::flow::index::StepState;

use super::harness::client;
use crate::data::needs::seed::{NeedSeed, PumpSeed, seed_need, seed_pump};
use crate::test_support::{SeedRun, Seeded, seeded};

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn seed_three(store: &Seeded) {
    seed_need(store, &NeedSeed::default());
    seed_need(
        store,
        &NeedSeed {
            handle: "5c20e9b7",
            subject: "parser: <b>trailing</b> commas",
            created_at: "2026-11-14T09:00:00+00:00",
            ..NeedSeed::default()
        },
    );
    seed_need(
        store,
        &NeedSeed {
            handle: "9e04a6f1",
            subject: "old finished work",
            state: "done",
            created_at: "2026-11-13T09:00:00+00:00",
            ..NeedSeed::default()
        },
    );
}

#[tokio::test]
async fn the_list_shows_open_needs_with_attempts_and_escapes_text() {
    let store = seeded(true);
    seed_three(&store);
    store.insert(&SeedRun {
        job: "job-1",
        need: Some("a1f3c2d0"),
        dispatched_at: now() - 900,
        collected_at: Some(now() - 300),
        outcome: Some("finished"),
        agent_rc: Some(0),
        check_rc: Some(1),
        ..SeedRun::default()
    });
    let page = client(&store).await.get("/ui/p/demo/needs").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("/ui/p/demo/needs/a1f3c2d0"));
    assert!(page.body.contains("clif: lower tuple returns"));
    assert!(
        !page.body.contains("old finished work"),
        "done needs are hidden by default"
    );
    assert!(page.body.contains("check rc 1"), "{}", page.body);
    assert!(
        page.body
            .contains("parser: &lt;b&gt;trailing&lt;/b&gt; commas")
    );
    assert!(!page.body.contains("<b>trailing"));
    assert!(
        page.body.contains("no pump"),
        "the missing pump is said plainly"
    );
}

#[tokio::test]
async fn the_state_filter_picks_done_and_any() {
    let store = seeded(true);
    seed_three(&store);
    let app = client(&store).await;
    let done = app.get("/ui/p/demo/needs?state=done").await;
    assert!(done.body.contains("old finished work"));
    assert!(!done.body.contains("clif: lower tuple returns"));
    let any = app.get("/ui/p/demo/needs?state=any").await;
    assert!(any.body.contains("old finished work"));
    assert!(any.body.contains("clif: lower tuple returns"));
}

fn seed_flow_positions(store: &Seeded) -> PumpSeed {
    seed_three(store);
    let pump = seed_pump(store);
    pump.run("a1f3c2d0", 1, "build", "plan", "live");
    pump.step("a1f3c2d0", 1, "sentinel", "t-sentinel", StepState::Running);
    pump.step("a1f3c2d0", 1, "build1", "t-build", StepState::Running);
    pump.job("a1f3c2d0", 1, "build1", "t-build", "job-step", 1);
    store.insert(&SeedRun {
        job: "job-step",
        need: Some("a1f3c2d0"),
        dispatched_at: now() - 3 * 3600,
        ..SeedRun::default()
    });
    pump.run("5c20e9b7", 1, "research", "research", "live");
    pump.step("5c20e9b7", 1, "sentinel", "u-sentinel", StepState::Running);
    pump.step("5c20e9b7", 1, "research", "u-research", StepState::Running);
    pump
}

#[tokio::test]
async fn flow_positions_counts_and_filters() {
    let store = seeded(true);
    let _pump = seed_flow_positions(&store);
    let app = client(&store).await;
    let page = app.get("/ui/p/demo/needs").await;
    assert!(page.body.contains("by step:"));
    assert!(page.body.contains("build1 1"), "{}", page.body);
    assert!(page.body.contains("research 1"));
    assert!(!page.body.contains("no pump"));

    let kind = app.get("/ui/p/demo/needs?kind=research").await;
    assert!(kind.body.contains("/ui/p/demo/needs/5c20e9b7"));
    assert!(!kind.body.contains("/ui/p/demo/needs/a1f3c2d0"));

    let step = app.get("/ui/p/demo/needs?step=build1").await;
    assert!(step.body.contains("/ui/p/demo/needs/a1f3c2d0"));
    assert!(!step.body.contains("/ui/p/demo/needs/5c20e9b7"));
    assert!(
        step.body.contains("research 1"),
        "counts ignore the step filter"
    );
}

#[tokio::test]
async fn the_stuck_filter_keeps_steps_older_than_the_hours_given() {
    let store = seeded(true);
    let _pump = seed_flow_positions(&store);
    let app = client(&store).await;
    let old = app.get("/ui/p/demo/needs?stuck=2").await;
    assert!(old.body.contains("/ui/p/demo/needs/a1f3c2d0"));
    assert!(
        !old.body.contains("/ui/p/demo/needs/5c20e9b7"),
        "no job, no start time"
    );
    let none = app.get("/ui/p/demo/needs?stuck=5").await;
    assert!(none.body.contains("No needs match."));
    let nonsense = app.get("/ui/p/demo/needs?stuck=abc").await;
    assert!(nonsense.body.contains("/ui/p/demo/needs/a1f3c2d0"));
}

#[tokio::test]
async fn an_unreadable_pump_is_named_and_the_list_still_shows() {
    let store = seeded(true);
    seed_three(&store);
    drop(seed_pump(&store));
    if store.root.join(".vivi/pump/pump.db-shm").exists() {
        return; // the platform left the WAL index behind: nothing to refuse
    }
    let page = client(&store).await.get("/ui/p/demo/needs").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(
        page.body.contains("cannot be read right now"),
        "{}",
        page.body
    );
    assert!(page.body.contains("clif: lower tuple returns"));
}

#[tokio::test]
async fn a_project_with_no_needs_or_no_store_says_so() {
    let store = seeded(true);
    let app = client(&store).await;
    let page = app.get("/ui/p/demo/needs").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("No needs match."));
}
