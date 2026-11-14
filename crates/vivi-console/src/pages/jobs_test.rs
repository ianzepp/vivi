use axum::http::StatusCode;

use crate::pages::test_support::get;
use crate::test_support::{SeedRun, Seeded, seeded};

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn finished(job: &str, age: i64) -> SeedRun<'_> {
    SeedRun {
        job,
        slot: "slot-1",
        dispatched_at: now() - age,
        collected_at: Some(now() - age + 60),
        outcome: Some("finished"),
        agent_rc: Some(0),
        check_rc: Some(0),
        job_s: Some(1800),
        ..SeedRun::default()
    }
}

fn seed(store: &Seeded) {
    store.insert(&SeedRun {
        job: "job-open",
        dispatched_at: now() - 100,
        need: Some("a1f3c2d0ff"),
        ..SeedRun::default()
    });
    store.insert(&finished("job-good", 200));
    store.insert(&SeedRun {
        check_rc: Some(101),
        pool: "other",
        slot: "slot-7",
        model: "<b>sneaky</b>",
        ..finished("job-red", 300)
    });
    store.insert(&SeedRun {
        outcome: Some("timeout"),
        ..finished("job-timeout", 400)
    });
    store.insert(&finished("job-old", 3 * 24 * 3600));
}

async fn page(store: &Seeded, query: &str) -> String {
    let (status, body) = get(&store.root, &format!("/ui/p/demo/jobs{query}")).await;
    assert_eq!(status, StatusCode::OK, "{query}");
    body
}

fn order(body: &str, jobs: &[&str]) {
    let at: Vec<usize> = jobs
        .iter()
        .map(|job| {
            body.find(&format!("jobs/{job}\""))
                .unwrap_or_else(|| panic!("{job} is not listed"))
        })
        .collect();
    assert!(at.windows(2).all(|pair| pair[0] < pair[1]), "{jobs:?}");
}

#[tokio::test]
async fn the_list_shows_columns_links_and_newest_first() {
    let store = seeded(true);
    seed(&store);
    let body = page(&store, "").await;
    order(&body, &["job-open", "job-good", "job-red", "job-timeout"]);
    assert!(!body.contains("jobs/job-old\""), "outside the 24h window");
    for text in [
        "Dispatched",
        "Machine",
        "studio-01",
        "model-a",
        "a1f3c2d0",
        "01234567",
        "href=\"/ui/p/demo/slots/main/slot-1\"",
        "check rc 101",
        "running",
        "30m",
    ] {
        assert!(body.contains(text), "{text}");
    }
    assert!(body.contains("href=\"/ui/p/demo/jobs?need=a1f3c2d0\""));
}

#[tokio::test]
async fn filters_in_the_query_string_narrow_the_list() {
    let store = seeded(true);
    seed(&store);
    let listed = |body: &str| -> Vec<&'static str> {
        ["job-open", "job-good", "job-red", "job-timeout", "job-old"]
            .into_iter()
            .filter(|job| body.contains(&format!("jobs/{job}\"")))
            .collect()
    };
    assert_eq!(
        listed(&page(&store, "?outcome=timeout").await),
        ["job-timeout"]
    );
    assert_eq!(listed(&page(&store, "?outcome=open").await), ["job-open"]);
    assert_eq!(
        listed(&page(&store, "?failed=1").await),
        ["job-red", "job-timeout"]
    );
    assert_eq!(listed(&page(&store, "?pool=other").await), ["job-red"]);
    assert_eq!(listed(&page(&store, "?slot=slot-7").await), ["job-red"]);
    assert_eq!(listed(&page(&store, "?need=a1f3").await), ["job-open"]);
    assert_eq!(
        listed(&page(&store, "?model=model-a&outcome=finished").await),
        ["job-good"]
    );
    assert_eq!(listed(&page(&store, "?window=1h").await).len(), 4);
    assert_eq!(listed(&page(&store, "?window=7d").await).len(), 5);
}

#[tokio::test]
async fn blank_and_bad_values_are_ignored_not_refused() {
    let store = seeded(true);
    seed(&store);
    let body = page(
        &store,
        "?outcome=&pool=&slot=%20&window=9y&before=x&failed=0",
    )
    .await;
    order(&body, &["job-open", "job-good", "job-red", "job-timeout"]);
    let bogus = page(&store, "?outcome=bogus").await;
    assert!(bogus.contains("jobs/job-good\""));
}

#[tokio::test]
async fn the_form_keeps_the_current_filters() {
    let store = seeded(true);
    seed(&store);
    let body = page(&store, "?outcome=finished&failed=1&model=model-a&window=7d").await;
    assert!(body.contains("<option value=\"finished\" selected=\"\">"));
    assert!(body.contains("<option value=\"7d\" selected=\"\">"));
    assert!(body.contains("name=\"failed\" value=\"1\" checked=\"\""));
    assert!(body.contains("name=\"model\" value=\"model-a\""));
}

#[tokio::test]
async fn nothing_matching_says_so() {
    let store = seeded(true);
    seed(&store);
    let body = page(&store, "?pool=ghost").await;
    assert!(body.contains("No jobs match"));
}

#[tokio::test]
async fn a_second_page_follows_the_cursor_without_gaps() {
    let store = seeded(true);
    for n in 0..130 {
        store.insert(&SeedRun {
            job: &format!("job-{n:03}"),
            dispatched_at: now() - 1000 - n / 2,
            ..SeedRun::default()
        });
    }
    let first = page(&store, "?outcome=open").await;
    assert_eq!(first.matches("/jobs/job-").count(), 100);
    let marker = "href=\"/ui/p/demo/jobs?outcome=open&amp;before=";
    let start = first.find(marker).expect("an older link") + "href=\"".len();
    let end = start + first[start..].find('"').unwrap();
    let link = first[start..end].replace("&amp;", "&");
    assert!(link.contains("&before_id="), "{link}");
    let second = {
        let (status, body) = get(&store.root, &link).await;
        assert_eq!(status, StatusCode::OK);
        body
    };
    assert_eq!(second.matches("/jobs/job-").count(), 30);
    assert!(second.contains(">newest</a>"));
    assert!(!second.contains(">older</a>"));
    assert!(first.contains("jobs/job-000\"") && second.contains("jobs/job-129\""));
}

#[tokio::test]
async fn model_text_is_escaped() {
    let store = seeded(true);
    seed(&store);
    let body = page(&store, "").await;
    assert!(!body.contains("<b>sneaky</b>"));
    assert!(body.contains("&lt;b&gt;sneaky&lt;/b&gt;"));
}

#[tokio::test]
async fn a_store_without_slot_runs_gets_the_friendly_page() {
    let store = seeded(false);
    let body = page(&store, "").await;
    assert!(body.contains("no slot index"));
}
