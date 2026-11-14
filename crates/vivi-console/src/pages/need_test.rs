use axum::http::StatusCode;
use vivi_core::pump::flow::index::StepState;

use crate::data::needs::seed::{NeedSeed, PumpSeed, content_of, seed_need, seed_pump, seed_reply};
use crate::pages::needs::harness::client;
use crate::test_support::{SeedRun, Seeded, seeded};

const URL: &str = "/ui/p/demo/needs/a1f3c2d0";

fn seed_thread(store: &Seeded) {
    seed_need(store, &NeedSeed::default());
    let parent = content_of("a1f3c2d0");
    seed_reply(
        store,
        &parent,
        "r2",
        "2026-11-14T10:00:00+00:00",
        "second <script>x</script>",
        false,
    );
    seed_reply(
        store,
        &parent,
        "r1",
        "2026-11-14T09:00:00+00:00",
        "first progress note",
        false,
    );
    seed_reply(
        store,
        &parent,
        "ev",
        "2026-11-14T09:30:00+00:00",
        "marked it",
        true,
    );
}

fn seed_attempts(store: &Seeded) {
    let now = chrono::Utc::now().timestamp();
    store.insert(&SeedRun {
        job: "job-a",
        need: Some("a1f3c2d0"),
        dispatched_at: now - 5000,
        collected_at: Some(now - 4000),
        outcome: Some("timeout"),
        ..SeedRun::default()
    });
    store.insert(&SeedRun {
        job: "job-b",
        need: Some("a1f3c2d0"),
        dispatched_at: now - 3000,
        collected_at: Some(now - 2000),
        outcome: Some("finished"),
        agent_rc: Some(0),
        check_rc: Some(0),
        landing: Some("landed"),
        ..SeedRun::default()
    });
}

#[tokio::test]
async fn the_attempts_tab_shows_done_when_thread_and_attempts_newest_first() {
    let store = seeded(true);
    seed_thread(&store);
    seed_attempts(&store);
    let page = client(&store).await.get(URL).await;
    assert_eq!(page.status, StatusCode::OK);
    let body = &page.body;
    assert!(body.contains("clif: lower tuple returns"));
    assert!(
        body.contains("[ ] </span>HIR lowers &amp; | ^") || body.contains("[ ] "),
        "{body}"
    );
    assert!(body.contains("register test passes"));
    assert!(body.contains("The brief prose."));
    assert!(body.contains("target: vivi"));
    // Replies, oldest first; lifecycle records are not replies.
    let first = body.find("first progress note").unwrap();
    let second = body.find("second &lt;script&gt;x&lt;/script&gt;").unwrap();
    assert!(first < second);
    assert!(!body.contains("marked it"));
    assert!(body.contains("1 lifecycle record(s)"));
    // Attempts, newest first, linked to the job page.
    let newest = body.find("/ui/p/demo/jobs/job-b").unwrap();
    let older = body.find("/ui/p/demo/jobs/job-a").unwrap();
    assert!(newest < older);
    assert!(body.contains("timeout") && body.contains("landed"));
    assert!(body.contains("2 attempt(s)"));
}

#[tokio::test]
async fn done_when_boxes_are_ticked_only_for_a_done_need() {
    let store = seeded(true);
    seed_need(
        &store,
        &NeedSeed {
            state: "done",
            ..NeedSeed::default()
        },
    );
    let page = client(&store).await.get(URL).await;
    assert!(page.body.contains("[x] "));
    assert!(!page.body.contains("[ ] "));
}

#[tokio::test]
async fn a_need_with_unreadable_front_matter_says_so() {
    let store = seeded(true);
    seed_need(
        &store,
        &NeedSeed {
            body: "+++\ndone_when = nope\n+++\nprose",
            ..NeedSeed::default()
        },
    );
    let page = client(&store).await.get(URL).await;
    assert!(page.body.contains("The front matter could not be read"));
    assert!(page.body.contains("prose"));
}

#[tokio::test]
async fn an_unknown_need_is_a_404() {
    let store = seeded(true);
    let page = client(&store).await.get("/ui/p/demo/needs/ffffffff").await;
    assert_eq!(page.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_need_only_the_slot_index_knows_still_shows_its_attempts() {
    let store = seeded(true);
    seed_attempts(&store);
    let page = client(&store).await.get(URL).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("Not in the backlog graph"));
    assert!(page.body.contains("/ui/p/demo/jobs/job-b"));
}

fn seed_flow(store: &Seeded) -> PumpSeed {
    seed_need(store, &NeedSeed::default());
    let pump = seed_pump(store);
    pump.run("a1f3c2d0", 1, "build", "research", "restarting");
    pump.step_verdict(
        "a1f3c2d0",
        1,
        "research",
        "t1-r",
        StepState::Done,
        Some("failed"),
    );
    pump.step_verdict("a1f3c2d0", 1, "cto", "t1-c", StepState::Done, Some("plan"));
    pump.store.set_restart_entry("a1f3c2d0", 1, "plan").unwrap();
    pump.run("a1f3c2d0", 2, "build", "plan", "live");
    pump.step("a1f3c2d0", 2, "sentinel", "t2-s", StepState::Running);
    pump.step_verdict(
        "a1f3c2d0",
        2,
        "plan",
        "t2-p",
        StepState::Done,
        Some("planned"),
    );
    pump.step("a1f3c2d0", 2, "build1", "t2-b", StepState::Running);
    pump.slice("a1f3c2d0", 2, "s1", "t2-b", &["radix/<call>.rs"]);
    pump.job("a1f3c2d0", 2, "slice:s1", "task-s1", "job-s1", 1);
    pump.rung("task-s1", "task", 1000, Some(1180), Some(0));
    pump.plan("a1f3c2d0", 1, "revise", "split abi from call lowering", 800);
    pump.branch("a1f3c2d0", "vivi/need/a1f3c2d0");
    pump
}

#[tokio::test]
async fn the_flow_tab_shows_the_path_slices_plans_and_branch() {
    let store = seeded(true);
    let _pump = seed_flow(&store);
    let page = client(&store).await.get(&format!("{URL}?tab=flow")).await;
    assert_eq!(page.status, StatusCode::OK);
    let body = &page.body;
    assert!(body.contains("Run 2 (entry: plan)"), "{body}");
    assert!(
        body.contains("[plan] -planned-&gt; [build1] (now)"),
        "{body}"
    );
    assert!(body.contains("Slices of build1"));
    assert!(body.contains("radix/&lt;call&gt;.rs"));
    assert!(body.contains("/ui/p/demo/jobs/job-s1"));
    assert!(body.contains("pass"));
    assert!(body.contains("Plan revisions"));
    assert!(body.contains("split abi from call lowering"));
    assert!(body.contains("vivi/need/a1f3c2d0"));
    assert!(body.contains("restart sent it to plan"));
    assert!(body.contains("tab=flow&amp;run=1"));
    assert!(
        !body.contains("flowchart TD"),
        "the source is behind its own link"
    );
}

#[tokio::test]
async fn an_earlier_run_and_the_whole_flow_can_be_asked_for() {
    let store = seeded(true);
    let _pump = seed_flow(&store);
    let app = client(&store).await;
    let first = app.get(&format!("{URL}?tab=flow&run=1")).await;
    assert!(first.body.contains("Run 1 (entry: research)"));
    assert!(first.body.contains("[research] -failed-&gt; [cto] (now)"));
    let source = app.get(&format!("{URL}?tab=flow&run=2&source=1")).await;
    assert!(source.body.contains("flowchart TD"));
    assert!(source.body.contains("Nodes this run visited: plan, build1"));
}

#[tokio::test]
async fn the_flow_tab_says_plainly_when_there_is_no_pump() {
    let store = seeded(true);
    seed_need(&store, &NeedSeed::default());
    let page = client(&store).await.get(&format!("{URL}?tab=flow")).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("no pump configured"));
}

#[tokio::test]
async fn a_need_that_never_ran_the_flow_says_its_work_went_to_slots() {
    let store = seeded(true);
    seed_need(&store, &NeedSeed::default());
    let _pump = seed_pump(&store);
    let page = client(&store).await.get(&format!("{URL}?tab=flow")).await;
    assert!(
        page.body.contains(
            "This need has no flow runs; its work was dispatched straight into pool slots."
        )
    );
}

#[tokio::test]
async fn an_unreadable_pump_store_is_named() {
    let store = seeded(true);
    seed_need(&store, &NeedSeed::default());
    drop(seed_pump(&store));
    if store.root.join(".vivi/pump/pump.db-shm").exists() {
        return; // the platform left the WAL index behind: nothing to refuse
    }
    let page = client(&store).await.get(&format!("{URL}?tab=flow")).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(
        page.body.contains("cannot be read right now"),
        "{}",
        page.body
    );
    assert!(!store.root.join(".vivi/pump/pump.db-shm").exists());
}

#[tokio::test]
async fn a_pump_that_has_written_no_store_says_so() {
    let store = seeded(true);
    seed_need(&store, &NeedSeed::default());
    let config = store.root.join(".vivi/mailspace.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    std::fs::write(&config, format!("{text}\n[pump]\n")).unwrap();
    let page = client(&store).await.get(&format!("{URL}?tab=flow")).await;
    assert!(page.body.contains("has not written its store yet"));
}
