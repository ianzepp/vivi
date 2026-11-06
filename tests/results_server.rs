//! The managed results database's submit path: run opening, validating rows,
//! refusal reasons, closing, and the `vivi-results` client against a server.

use std::path::{Path, PathBuf};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tempfile::TempDir;
use tower::util::ServiceExt;
use vivi_core::pool::job::RUNNER;
use vivi_core::results::{ResultRow, ResultsStore};
use vivi_server::server::results::{RunJob, local_host, open_job};
use vivi_server::server::{app_state, router};

struct Fx {
    dir: TempDir,
    app: Router,
    root: PathBuf,
    job: String,
    commit: String,
}

fn fixture() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("project");
    std::fs::create_dir_all(&root).unwrap();
    let state = app_state(dir.path().join("data"), None, None).expect("app state");
    let commit = "a".repeat(40);
    let metadata = RunJob {
        project: String::from("project-a"),
        role: String::from("check"),
        target: String::from("default"),
        commit: commit.clone(),
        host: local_host(),
        job: Some(String::from("job-1")),
        task: Some(String::from("task-1")),
    };
    let job = open_job(&state, &root, &metadata).unwrap();
    Fx {
        app: router(state),
        root,
        job,
        commit,
        dir,
    }
}

impl Fx {
    async fn post(&self, uri: &str, body: &str, suite: Option<&str>) -> (StatusCode, String) {
        let mut request = Request::builder().method("POST").uri(uri);
        if let Some(suite) = suite {
            request = request.header("x-vivi-suite", suite);
        }
        let response = self
            .app
            .clone()
            .oneshot(request.body(Body::from(body.to_owned())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    async fn open(&self, suite: &str) -> String {
        let (status, body) = self
            .post(&format!("/v1/results/open/{}", self.job), "", Some(suite))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let value: Value = serde_json::from_str(&body).unwrap();
        value["token"].as_str().unwrap().to_owned()
    }

    fn store(&self) -> ResultsStore {
        ResultsStore::open_project(&self.root).unwrap()
    }
}

fn row(case: &str, status: &str) -> ResultRow {
    ResultRow {
        case: case.to_owned(),
        status: status.to_owned(),
        phase: None,
        diagnostic: None,
        millis: None,
    }
}

fn line(row: &ResultRow) -> String {
    serde_json::to_string(row).unwrap()
}

#[tokio::test]
async fn a_job_opens_one_run_per_suite_and_posts_closes_and_refuses_again() {
    let fx = fixture();
    let unit = fx.open("unit").await;
    let rows = format!(
        "{}\n{}\n",
        line(&row("case-a", "pass")),
        line(&ResultRow {
            diagnostic: Some(String::from("first line\nignored")),
            millis: Some(7),
            ..row("case-b", "fail")
        })
    );
    let (status, body) = fx.post(&format!("/v1/results/{unit}"), &rows, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, "accepted: 2 rows\n");
    let (status, body) = fx
        .post(&format!("/v1/results/{unit}"), "{\"close\":true}\n", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, "accepted: closed\n");

    let store = fx.store();
    let run = store.run(&unit).unwrap().unwrap();
    assert_eq!(
        (
            run.project.as_str(),
            run.role.as_str(),
            run.target.as_str(),
            run.suite.as_str(),
            run.commit.as_str(),
        ),
        ("project-a", "check", "default", "unit", fx.commit.as_str())
    );
    assert!(run.finished_at.is_some());
    let stored = store.rows(&unit).unwrap();
    assert_eq!(stored.len(), 2);
    assert_eq!(stored[1].diagnostic.as_deref(), Some("first line"));

    let (status, body) = fx
        .post(
            &format!("/v1/results/{unit}"),
            &line(&row("late", "pass")),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.starts_with("rejected: "), "{body}");

    let integration = fx.open("integration").await;
    let (status, body) = fx
        .post(
            &format!("/v1/results/{integration}"),
            &line(&row("case-c", "pass")),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let runs = fx
        .store()
        .runs_for_commit("project-a", "default", "integration", &fx.commit)
        .unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].suite, "integration");
}

#[tokio::test]
async fn unknown_tokens_statuses_duplicates_and_oversized_fields_are_refused() {
    let fx = fixture();
    let unknown_job = format!("vivirj_{}", "a".repeat(64));
    let (status, body) = fx
        .post(&format!("/v1/results/open/{unknown_job}"), "", Some("unit"))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    let run = fx.open("unit").await;
    let unknown_run = format!("vivir_{}", "b".repeat(64));
    let (status, body) = fx
        .post(
            &format!("/v1/results/{unknown_run}"),
            &line(&row("case", "pass")),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    let (status, body) = fx
        .post(
            &format!("/v1/results/{run}"),
            &line(&row("case", "perhaps")),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body.contains("unknown status"), "{body}");

    let long = row(&"x".repeat(513), "pass");
    let (status, body) = fx
        .post(&format!("/v1/results/{run}"), &line(&long), None)
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body.contains("case is longer"), "{body}");

    let foreign = String::from("{\"case\":\"case\",\"status\":\"pass\",\"foreign\":true}\n");
    let (status, body) = fx.post(&format!("/v1/results/{run}"), &foreign, None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body.contains("unknown field"), "{body}");

    let (status, body) = fx
        .post(&format!("/v1/results/{run}"), "not json\n", None)
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body.contains("line 1"), "{body}");

    let (status, body) = fx
        .post(
            &format!("/v1/results/{run}"),
            &format!(
                "{}\n{}\n",
                line(&row("same", "pass")),
                line(&row("same", "fail"))
            ),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body.contains("submitted twice"), "{body}");

    let (status, body) = fx
        .post(
            &format!("/v1/results/{run}"),
            &line(&row("same", "pass")),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = fx
        .post(
            &format!("/v1/results/{run}"),
            &line(&row("same", "fail")),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body.contains("submitted twice"), "{body}");
}

fn client_script(dir: &Path) -> PathBuf {
    let start = RUNNER.find("# VIVI_RESULTS_CLIENT_START\n").unwrap();
    let end = RUNNER.find("# VIVI_RESULTS_CLIENT_END\n").unwrap();
    let script = &RUNNER[start..end];
    let path = dir.join("vivi-results");
    std::fs::write(&path, script).unwrap();
    path
}

async fn serve(fx: &Fx) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = fx.app.clone();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}"), task)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_client_posts_rows_in_batches_and_closes_the_run() {
    let fx = fixture();
    let (url, server) = serve(&fx).await;
    let script = client_script(fx.dir.path());
    let rows = fx.dir.path().join("rows.jsonl");
    std::fs::write(
        &rows,
        format!(
            "{}\n{}\n",
            line(&row("case-a", "pass")),
            line(&row("case-b", "skip"))
        ),
    )
    .unwrap();
    let output = std::process::Command::new("sh")
        .arg(&script)
        .arg("--suite")
        .arg("unit")
        .arg(&rows)
        .env("VIVI_RESULTS_URL", &url)
        .env("VIVI_RESULTS_TOKEN", &fx.job)
        .env("VIVI_RESULTS_BATCH", "1")
        .output()
        .unwrap();
    server.abort();
    let _ = server.await;
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "accepted: 2 rows\n"
    );
    let store = fx.store();
    let runs = store
        .runs_for_commit("project-a", "default", "unit", &fx.commit)
        .unwrap();
    assert_eq!(runs.len(), 1);
    assert!(runs[0].finished_at.is_some());
    let stored = store.rows(&runs[0].id).unwrap();
    assert_eq!(stored.len(), 2);
    assert_eq!(stored[1].status, "skip");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_client_prints_the_servers_refusal_reason() {
    let fx = fixture();
    let (url, server) = serve(&fx).await;
    let script = client_script(fx.dir.path());
    let rows = fx.dir.path().join("bad.jsonl");
    std::fs::write(&rows, format!("{}\n", line(&row("case", "maybe")))).unwrap();
    let output = std::process::Command::new("sh")
        .arg(&script)
        .arg("--suite=unit")
        .arg(&rows)
        .env("VIVI_RESULTS_URL", &url)
        .env("VIVI_RESULTS_TOKEN", &fx.job)
        .output()
        .unwrap();
    server.abort();
    let _ = server.await;
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.starts_with("rejected: "), "{stderr}");
    assert!(stderr.contains("unknown status"), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
}
