use axum::http::StatusCode;

use crate::pages::test_support::{Project, get, project_with_task};

const ANCHORS: [&str; 8] = [
    "brief", "check", "agent", "logs", "landing", "thread", "raw", "events",
];

const COLLECT: &str = r#"{"job":"job-1","pool":"main","slot":"slot-1","outcome":"finished","agent_rc":"0","agent_s":"1500","check_rc":"101","check_s":"300","job_s":"1800","push":"pushed","sha":"deadbeefdeadbeef0000","check_tail":"test result: FAILED. 3 passed; 1 failed <oops>","agent_cpu_s":"2400"}"#;

const RECEIPT: &str = "pool result job-1: finished\npool main  slot slot-1\nagent exit 0 in 1500s\n\n\
    --- receipt\nI fixed <b>the edge</b> case.\n\n--- stderr tail\nwarning: unused <x>\n\n\
    --- runner log tail\nrunner line 1\nupload token=sekrit12345 done";

async fn job_page(project: &Project, job: &str) -> (StatusCode, String) {
    get(&project.root, &format!("/ui/p/demo/jobs/{job}")).await
}

fn collected(project: &Project) {
    project.dispatch("job-1", chrono::Utc::now().timestamp() - 1900);
    project.record("pool_collected", COLLECT, RECEIPT);
}

#[tokio::test]
async fn a_collected_job_renders_every_section() {
    let project = project_with_task("Fix one done_when item: <script>alert(1)</script>");
    collected(&project);
    let (status, body) = job_page(&project, "job-1").await;
    assert_eq!(status, StatusCode::OK);
    for anchor in ANCHORS {
        assert!(body.contains(&format!("id=\"{anchor}\"")), "{anchor}");
    }
    for text in [
        "Job job-1",
        "a1f3c2d0",
        "pill pill-ok\">finished",
        "agent 0",
        "check 101",
        "main / slot-1",
        "studio-01",
        "model-a",
        "factory/x",
        "0123456789ab",
        "pushed deadbeefdead",
        "55m",
        "25m",
        "5m",
        "30m",
        "40m",
        "Fix one done_when item",
        "test result: FAILED. 3 passed; 1 failed &lt;oops&gt;",
        "I fixed &lt;b&gt;the edge&lt;/b&gt; case.",
        "warning: unused &lt;x&gt;",
        "runner line 1",
        "pool dispatch job-1",
        "pool_dispatched",
        "pool_collected",
    ] {
        assert!(body.contains(text), "{text}\n{body}");
    }
}

#[tokio::test]
async fn cpu_time_shows_only_when_recorded() {
    let project = project_with_task("brief");
    collected(&project);
    let (_, body) = job_page(&project, "job-1").await;
    assert!(body.contains("agent cpu"), "agent cpu is recorded");
    assert!(!body.contains("check cpu"), "check cpu is NULL");
}

#[tokio::test]
async fn text_from_the_store_is_escaped_and_secrets_are_not_shown() {
    let project = project_with_task("Fix one done_when item: <script>alert(1)</script>");
    collected(&project);
    let (_, body) = job_page(&project, "job-1").await;
    assert!(!body.contains("<script>alert"));
    assert!(body.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(!body.contains("<b>the edge</b>"));
    assert!(!body.contains("sekrit12345"));
    assert!(body.contains("token=[redacted]"));
}

#[tokio::test]
async fn a_job_not_collected_yet_still_renders() {
    let project = project_with_task("brief text");
    project.dispatch("job-1", chrono::Utc::now().timestamp() - 60);
    let (status, body) = job_page(&project, "job-1").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("pill-busy\">running"));
    assert!(body.contains("brief text"));
    assert!(body.contains("not collected"));
    assert!(body.contains("The collect record has no check output"));
    assert!(body.contains("Nothing has been recorded about landing"));
    assert!(body.contains("pool dispatch job-1"));
    assert!(!body.contains("agent cpu"));
    for anchor in ANCHORS {
        assert!(body.contains(&format!("id=\"{anchor}\"")), "{anchor}");
    }
}

fn mark_landing(project: &Project, word: &str, reason: &str) {
    let conn = rusqlite::Connection::open(project.root.join(".vivi/mail.sqlite")).unwrap();
    conn.execute(
        "UPDATE slot_runs SET landing = ?1, landing_at = 1700000000, landing_reason = ?2",
        rusqlite::params![word, reason],
    )
    .unwrap();
}

#[tokio::test]
async fn a_red_landing_shows_the_failing_command_and_its_output() {
    let project = project_with_task("brief");
    collected(&project);
    project.record(
        "pool_landed",
        r#"{"job":"job-1","landing":"red","base":"aaaaaaaaaaaaaaaa","tip":"bbbbbbbbbbbbbbbb","secs":42,"command":"cargo test -p <crate>","tail":"error: boom"}"#,
        "pool landed job-1: red",
    );
    mark_landing(&project, "red", "");
    let (_, body) = job_page(&project, "job-1").await;
    for text in [
        "pill pill-failed\">red",
        "failing command",
        "cargo test -p &lt;crate&gt;",
        "output tail",
        "error: boom",
        "base aaaaaaaaaaaa",
        "tip bbbbbbbbbbbb",
        "took 42s",
        "pool_landed",
    ] {
        assert!(body.contains(text), "{text}");
    }
    assert!(body.contains("<details id=\"landing\" class=\"section\" open=\"\">"));
}

#[tokio::test]
async fn a_conflict_lists_its_files() {
    let project = project_with_task("brief");
    collected(&project);
    project.record(
        "pool_landed",
        r#"{"job":"job-1","landing":"conflict","files":["src/a.rs","src/<b>.rs"]}"#,
        "pool landed job-1: conflict",
    );
    mark_landing(&project, "conflict", "");
    let (_, body) = job_page(&project, "job-1").await;
    assert!(body.contains("pill pill-conflict\">conflict"));
    assert!(body.contains("files in conflict"));
    assert!(body.contains("<li><span class=\"mono\">src/a.rs</span></li>"));
    assert!(body.contains("src/&lt;b&gt;.rs"));
}

#[tokio::test]
async fn a_landing_with_only_the_index_row_shows_its_reason() {
    let project = project_with_task("brief");
    collected(&project);
    mark_landing(&project, "raced", "main moved <twice>");
    let (_, body) = job_page(&project, "job-1").await;
    assert!(body.contains("pill pill-conflict\">raced"));
    assert!(body.contains("main moved &lt;twice&gt;"));
}

#[tokio::test]
async fn the_raw_section_pretty_prints_each_record_of_this_job_only() {
    let project = project_with_task("brief");
    collected(&project);
    project.dispatch("job-2", chrono::Utc::now().timestamp() - 10);
    let (_, body) = job_page(&project, "job-1").await;
    assert!(body.contains("&quot;check_tail&quot;: &quot;test result"));
    assert!(body.contains("&quot;start&quot;: &quot;0123456789abcdef0123&quot;"));
    assert!(body.contains("not recorded"), "no landing note yet");
    let other = body.matches("&quot;job&quot;: &quot;job-2&quot;").count();
    assert_eq!(other, 0, "job-2's dispatch note is not job-1's");
    let events = body.rfind("id=\"events\"").unwrap();
    assert!(
        body[events..].matches("<tr>").count() >= 4,
        "all of the task's events"
    );
}

#[tokio::test]
async fn an_unknown_job_is_a_404_page() {
    let project = project_with_task("brief");
    project.dispatch("job-1", chrono::Utc::now().timestamp());
    let (status, body) = job_page(&project, "no-such-job").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("No job with this name"));
}

#[tokio::test]
async fn a_store_without_slot_runs_gets_the_friendly_page() {
    let store = crate::test_support::seeded(false);
    let (status, body) = get(&store.root, "/ui/p/demo/jobs/job-1").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("no slot index"));
}
