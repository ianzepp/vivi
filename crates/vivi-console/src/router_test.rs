use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use tower::ServiceExt;

use super::*;
use crate::test_support::{FakeHost, SeedRun, Seeded, project_without_store, seeded};

fn app_for(catalog: crate::host::Catalog) -> Router {
    router(Arc::new(FakeHost { catalog }))
}

struct Reply {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: String,
}

impl Reply {
    fn header(&self, name: header::HeaderName) -> String {
        self.headers
            .get(name)
            .map(|value| value.to_str().unwrap().to_string())
            .unwrap_or_default()
    }
}

async fn send(app: &Router, method: Method, uri: &str, form: Option<&str>) -> Reply {
    let mut builder = Request::builder().method(method).uri(uri);
    if form.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    }
    let body = Body::from(form.unwrap_or("").to_string());
    let response = app
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let (parts, body) = response.into_parts();
    let bytes = to_bytes(body, 4 * 1024 * 1024).await.unwrap();
    Reply {
        status: parts.status,
        headers: parts.headers,
        body: String::from_utf8_lossy(&bytes).to_string(),
    }
}

async fn get(app: &Router, uri: &str) -> Reply {
    send(app, Method::GET, uri, None).await
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Two busy slots, two free ones, and failures of each flavour.
fn seed_runs(store: &Seeded) {
    let now = now();
    let base = SeedRun::default;
    store.insert(&SeedRun {
        job: "job-busy",
        slot: "slot-1",
        need: Some("a1f3c2d0ff"),
        model: "gpt-x",
        dispatched_at: now - 600,
        ..base()
    });
    store.insert(&SeedRun {
        job: "job-ok",
        slot: "slot-2",
        dispatched_at: now - 3000,
        collected_at: Some(now - 1200),
        outcome: Some("finished"),
        agent_rc: Some(0),
        check_rc: Some(0),
        job_s: Some(1800),
        landing: Some("landed"),
        ..base()
    });
    store.insert(&SeedRun {
        job: "job-red",
        slot: "slot-3",
        machine: "studio-02",
        dispatched_at: now - 4000,
        collected_at: Some(now - 900),
        outcome: Some("finished"),
        agent_rc: Some(0),
        check_rc: Some(101),
        job_s: Some(3100),
        ..base()
    });
    store.insert(&SeedRun {
        job: "job-timeout",
        slot: "slot-3",
        machine: "studio-02",
        model: "<b>sneaky</b>",
        dispatched_at: now - 9000,
        collected_at: Some(now - 5000),
        outcome: Some("timeout"),
        ..base()
    });
}

#[tokio::test]
async fn pages_open_without_sign_in() {
    let store = seeded(true);
    seed_runs(&store);
    let app = app_for(store.catalog());
    let home = send(&app, Method::GET, "/ui/", None).await;
    assert_eq!(home.status, StatusCode::SEE_OTHER);
    assert_eq!(home.header(header::LOCATION), "/ui/p/demo/overview");
    for uri in ["/ui/p/demo/overview", "/ui/p/demo/slots", "/ui/p/demo/jobs"] {
        let reply = send(&app, Method::GET, uri, None).await;
        assert_eq!(reply.status, StatusCode::OK, "{uri}");
        assert!(reply.headers.get(header::SET_COOKIE).is_none(), "{uri}");
    }
    let login = send(&app, Method::GET, "/ui/login", None).await;
    assert_eq!(login.status, StatusCode::SEE_OTHER, "an old sign-in link");
    assert_eq!(login.header(header::LOCATION), "/ui/");
    let nowhere = send(&app, Method::GET, "/ui/no/such/page", None).await;
    assert_eq!(nowhere.status, StatusCode::NOT_FOUND);
    assert_eq!(
        nowhere.header(header::CONTENT_TYPE),
        "text/html; charset=utf-8",
        "a typeless empty 404 is downloaded as a file by some browsers"
    );
}

#[tokio::test]
async fn an_unknown_project_is_a_404() {
    let store = seeded(true);
    let app = app_for(store.catalog());
    for uri in [
        "/ui/p/other/overview",
        "/ui/p/other/slots",
        "/ui/p/other/slots/main/slot-1",
        "/ui/p/other/jobs",
    ] {
        let reply = get(&app, uri).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{uri}");
    }
    // The project's id works too.
    let by_id = get(&app, "/ui/p/proj-1/overview").await;
    assert_eq!(by_id.status, StatusCode::OK);
}

#[tokio::test]
async fn the_overview_renders_a_seeded_project() {
    let store = seeded(true);
    seed_runs(&store);
    let app = app_for(store.catalog());
    let page = get(&app, "/ui/p/demo/overview").await;
    assert_eq!(page.status, StatusCode::OK);
    let body = &page.body;
    assert!(body.contains("no pump"), "no [pump] table in the fixture");
    assert!(body.contains("1 busy / 4"), "{body}");
    assert!(body.contains("3 free"));
    // 4 dispatched in 24 h; 3 collected: 1 ok, 2 failed (check red, timeout).
    assert!(
        body.contains("<div class=\"big\">4</div>"),
        "dispatched count"
    );
    assert!(body.contains("<span class=\"ok-num\">1</span>"));
    assert!(body.contains("<span class=\"fail-num\">2</span>"));
    assert!(body.contains("1 timeout"));
    assert!(body.contains("<svg class=\"chart\""));
    assert!(body.contains("class=\"bar-fail\""));
    // Newest failures link to their job page; the check-red one is newest.
    assert!(body.contains("href=\"/ui/p/demo/jobs/job-red\""));
    assert!(body.contains("href=\"/ui/p/demo/jobs/job-timeout\""));
    assert!(body.find("job-red").unwrap() < body.find("job-timeout").unwrap());
    assert!(body.contains("check rc 101"));
    assert!(
        !body.contains("job-ok"),
        "a finished, clean run is not a failure"
    );
    // Page chrome: menu, picker, logout.
    for label in [
        "Overview",
        "Slots",
        "Jobs",
        "Needs",
        "Failures",
        "Pump",
        "Pools and roles",
        "Mail",
    ] {
        assert!(body.contains(&format!(">{label}</a>")), "{label}");
    }
    assert!(!body.contains("/ui/logout"), "there is no sign-out");
}

#[tokio::test]
async fn the_slots_page_shows_tiles_rows_and_filters() {
    let store = seeded(true);
    seed_runs(&store);
    let app = app_for(store.catalog());
    let page = get(&app, "/ui/p/demo/slots").await;
    assert_eq!(page.status, StatusCode::OK);
    let body = &page.body;
    assert!(body.contains("tile tile-busy"));
    assert!(body.contains("tile tile-free"));
    assert!(body.contains("href=\"/ui/p/demo/slots/main/slot-1\""));
    assert!(body.contains("studio-01") && body.contains("studio-02"));
    assert!(body.contains("a1f3c2d0"), "need handle");
    assert!(body.contains("01234567"), "task handle");
    assert!(body.contains("gpt-x"));
    assert!(body.contains("class=\"meter\""));
    assert!(
        body.contains("1 declared slot(s) in pool main"),
        "4 declared, 3 named"
    );

    let filtered = get(&app, "/ui/p/demo/slots?machine=studio-02").await;
    assert!(filtered.body.contains("slots/main/slot-3"));
    assert!(!filtered.body.contains("slots/main/slot-1\""));
    let none = get(&app, "/ui/p/demo/slots?pool=ghost").await;
    assert!(none.body.contains("No slots match"));
}

#[tokio::test]
async fn a_slot_history_lists_newest_first() {
    let store = seeded(true);
    seed_runs(&store);
    let app = app_for(store.catalog());
    let page = get(&app, "/ui/p/demo/slots/main/slot-3").await;
    assert_eq!(page.status, StatusCode::OK);
    let body = &page.body;
    assert!(body.find("job-red").unwrap() < body.find("job-timeo").unwrap());
    assert!(body.contains("0 / 101"));
    assert!(
        body.contains("1h 11m") || body.contains("51m"),
        "wall time: {body}"
    );
    let empty = get(&app, "/ui/p/demo/slots/main/slot-99").await;
    assert!(empty.body.contains("No runs are recorded"));
}

#[tokio::test]
async fn text_from_the_store_is_escaped() {
    let store = seeded(true);
    seed_runs(&store);
    let app = app_for(store.catalog());
    let page = get(&app, "/ui/p/demo/slots/main/slot-3").await;
    assert!(!page.body.contains("<b>sneaky</b>"));
    assert!(page.body.contains("&lt;b&gt;sneaky&lt;/b&gt;"));
}

#[tokio::test]
async fn a_store_without_slot_runs_gets_a_friendly_page() {
    let store = seeded(false);
    let app = app_for(store.catalog());
    for uri in [
        "/ui/p/demo/overview",
        "/ui/p/demo/slots",
        "/ui/p/demo/slots/main/slot-1",
    ] {
        let page = get(&app, uri).await;
        assert_eq!(page.status, StatusCode::OK, "{uri}");
        assert!(page.body.contains("no slot index"), "{uri}: {}", page.body);
    }
    // The overview still shows what config declares.
    let overview = get(&app, "/ui/p/demo/overview").await;
    assert!(overview.body.contains("4 declared"));
}

#[tokio::test]
async fn a_project_with_no_store_yet_gets_a_friendly_page() {
    let (_dir, root) = project_without_store();
    let catalog = crate::host::Catalog {
        projects: vec![crate::host::ProjectRef {
            id: "proj-1".into(),
            name: "demo".into(),
            mailspace_root: root,
        }],
    };
    let app = app_for(catalog);
    let page = get(&app, "/ui/p/demo/slots").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("no mailspace store yet"));
}

#[tokio::test]
async fn every_menu_page_is_built() {
    assert!(crate::pages::unbuilt::UNBUILT.is_empty());
    let store = seeded(true);
    let app = app_for(store.catalog());
    for segment in [
        "overview", "slots", "jobs", "needs", "failures", "pump", "pools", "mail",
    ] {
        let page = get(&app, &format!("/ui/p/demo/{segment}")).await;
        assert_eq!(page.status, StatusCode::OK, "{segment}");
        assert!(!page.body.contains("not built yet"), "{segment}");
    }
}

#[tokio::test]
async fn static_files_and_security_headers() {
    let store = seeded(true);
    let app = app_for(store.catalog());
    let css = send(&app, Method::GET, "/ui/static/console.css", None).await;
    assert_eq!(css.status, StatusCode::OK);
    assert!(css.header(header::CONTENT_TYPE).starts_with("text/css"));
    assert!(css.body.contains("prefers-color-scheme: dark"));
    let js = send(&app, Method::GET, "/ui/static/console.js", None).await;
    assert_eq!(js.status, StatusCode::OK);
    let login = send(&app, Method::GET, "/ui/login", None).await;
    assert_eq!(login.header(header::CACHE_CONTROL), "no-store");
    assert!(
        login
            .header(HeaderName::from_static("content-security-policy"))
            .contains("default-src 'none'")
    );
    assert_eq!(
        login.header(HeaderName::from_static("x-content-type-options")),
        "nosniff"
    );
}

#[tokio::test]
async fn a_key_with_no_projects_sees_a_plain_page() {
    let app = app_for(crate::host::Catalog {
        projects: Vec::new(),
    });
    let page = get(&app, "/ui/").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("No projects"));
}
