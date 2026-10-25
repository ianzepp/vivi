//! Shared fixture for the archive lifecycle: archive, restore, purge
//! timing, and purge cleanup (`tests/host_archive/*.rs`).
//!
//! The harness follows the isolated `VIVI_CONFIG_DIR` + scratch
//! `HostConfig` pattern of `tests/host_create_project.rs`: one config
//! directory per test binary (the host registry is process-global) and a
//! scratch data/projects/lanes tree per fixture. Every case must create
//! its projects under a distinct safe name so process-global state never
//! collides.
//!
//! The fixture's `ProjectStore` reads a fixed whole-second Unix time from
//! `Fx::clock` (`Arc<AtomicI64>`) through `ProjectStore::with_clock`.
//! Moving that atomic is the only mechanism later lifecycle tests use to
//! simulate elapsed time; no archive timestamp is altered in metadata and
//! no environment variable delays anything.
//!
//! Helpers are exposed to the child modules by ordinary descendant
//! visibility (a child module reads its ancestors' private items); no
//! production API is changed for tests.

// The lifecycle cases in the child modules drive the helpers this file
// provides; until those tests land some of them are unused.
#![allow(dead_code)]

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Once;
use std::sync::atomic::{AtomicI64, Ordering};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use chrono::{SecondsFormat, TimeZone, Utc};
use serde_json::{Value, json};
use tower::util::ServiceExt;
use vivi_server::mcp::host::HostConfig;
use vivi_server::projects::ProjectStore;
use vivi_server::{AppState, app_state, router};

#[path = "host_archive/archive_cases.rs"]
mod archive_cases;
#[path = "host_archive/purge_cleanup.rs"]
mod purge_cleanup;
#[path = "host_archive/purge_timing.rs"]
mod purge_timing;
#[path = "host_archive/restore_cases.rs"]
mod restore_cases;

const TOKEN: &str = "host-operator-secret";
const PREFIX: &str = "git@hub.example.ts.net:";

/// The fixed whole-second Unix instant every fixture starts at, far from
/// any real now.
const BASE_SECONDS: i64 = 1_792_000_000;

static ENV: Once = Once::new();

/// One scratch config directory for the whole test binary: the host
/// registry is process-global (`VIVI_CONFIG_DIR`). Every test uses its own
/// names.
fn isolate_registry() {
    ENV.call_once(|| {
        let dir = std::env::temp_dir().join(format!(
            "vivi-server-host-archive-cfg-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // SAFETY: runs once, before any test reads the environment.
        unsafe { std::env::set_var("VIVI_CONFIG_DIR", &dir) };
    });
}

struct Fx {
    app: Router,
    state: AppState,
    projects: PathBuf,
    lanes: PathBuf,
    data: PathBuf,
    /// The store's clock in whole Unix seconds; store it forward to
    /// simulate elapsed time.
    clock: Arc<AtomicI64>,
}

fn fixture(name: &str) -> Fx {
    isolate_registry();
    let base = std::env::temp_dir().join(format!(
        "vivi-server-host-archive-{}-{name}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&base);
    let (projects, lanes) = (base.join("projects"), base.join("lanes"));
    let data = base.join("data");
    fs::create_dir_all(&projects).unwrap();
    fs::create_dir_all(&lanes).unwrap();
    let clock = Arc::new(AtomicI64::new(BASE_SECONDS));
    let reading = clock.clone();
    let mut state = app_state(data.clone(), None, None);
    state.projects = ProjectStore::with_clock(&data, move || {
        Utc.timestamp_opt(reading.load(Ordering::SeqCst), 0)
            .unwrap()
    });
    state.host_operator_token = Some(TOKEN.into());
    state.host = HostConfig {
        projects_root: Some(projects.clone()),
        lanes_root: Some(lanes.clone()),
        git_url_prefix: Some(PREFIX.into()),
        default_pool: None,
    };
    Fx {
        app: router(state.clone()),
        state,
        projects,
        lanes,
        data,
        clock,
    }
}

/// The fixture instant (`seconds` Unix seconds after the epoch) as the
/// whole-second RFC 3339 UTC string the server stamps write.
fn rfc3339(seconds: i64) -> String {
    Utc.timestamp_opt(seconds, 0)
        .unwrap()
        .to_rfc3339_opts(SecondsFormat::Secs, true)
}

impl Fx {
    async fn post(&self, uri: &str, bearer: Option<&str>, body: Value) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json");
        if let Some(bearer) = bearer {
            request = request.header("authorization", format!("Bearer {bearer}"));
        }
        let response = self
            .app
            .clone()
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// The tool's own JSON and whether it errored; HTTP transport must be
    /// healthy (`200 OK`, a `content` array, a text part).
    async fn call_as(&self, bearer: &str, name: &str, arguments: Value) -> (bool, Value) {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": name, "arguments": arguments}});
        let (status, value) = self.post("/v1/mcp", Some(bearer), body).await;
        assert_eq!(status, StatusCode::OK, "{name}: {value}");
        let text = value["result"]["content"][0]["text"].as_str().unwrap();
        (
            value["result"]["isError"] == true,
            serde_json::from_str(text).unwrap(),
        )
    }

    /// A host-operator call: the configured token, the tool's own JSON and
    /// its `isError` flag.
    async fn call_tool(&self, name: &str, arguments: Value) -> (bool, Value) {
        self.call_as(TOKEN, name, arguments).await
    }

    /// `create_project` through the host operator token.
    async fn create(&self, arguments: Value) -> (bool, Value) {
        self.call_tool("create_project", arguments).await
    }

    /// `DATA_DIR/projects/<project_id>/project.json` as it lies on disk;
    /// the mailspace itself lives under the host projects root.
    fn project_json(&self, project_id: &str) -> Value {
        let bytes = fs::read(
            self.data
                .join("projects")
                .join(project_id)
                .join("project.json"),
        )
        .expect("project.json exists");
        serde_json::from_slice(&bytes).expect("project.json parses")
    }

    /// Hold the project's real `.vivi/pump/lock` until the returned `File`
    /// drops; later lifecycle cases use this to prove the archive and
    /// purge tools refuse while the pump lock is held.
    fn hold_pump_lock(&self, name: &str) -> std::fs::File {
        let home = self.projects.join(name).join(".vivi/pump");
        fs::create_dir_all(&home).unwrap();
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(home.join("lock"))
            .unwrap();
        lock.try_lock().unwrap();
        lock
    }
}

/// Relative path and size of every file under `tree`, sorted by path, so a
/// before/after comparison can diff a whole project tree.
fn snapshot(tree: &Path) -> Vec<(String, u64)> {
    let mut files = Vec::new();
    fn walk(tree: &Path, prefix: &str, files: &mut Vec<(String, u64)>) {
        for entry in fs::read_dir(tree).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let rel = if prefix.is_empty() {
                entry.file_name().to_string_lossy().into_owned()
            } else {
                format!("{prefix}/{}", entry.file_name().to_string_lossy())
            };
            if entry.file_type().unwrap().is_dir() {
                walk(&path, &rel, files);
            } else {
                files.push((rel, path.metadata().unwrap().len()));
            }
        }
    }
    walk(tree, "", &mut files);
    files.sort();
    files
}

/// The fixture serves a created project end to end: `create_project` with
/// an initial commit, and the minted operator key alone reaches
/// `pump_list` on `/v1/mcp`. No archive tool is called; this smoke keeps
/// the foundation mergeable before the lifecycle cases arrive.
#[tokio::test]
async fn fixture_serves_a_project_and_the_minted_key_reaches_pump_list() {
    let fx = fixture("smoke");
    let (errored, made) = fx
        .create(json!({"name": "archive-smoke", "initial_commit": true}))
        .await;
    assert!(!errored, "{made}");
    assert_eq!(made["served"], true, "{made}");
    assert!(made["initial_commit"].is_string(), "{made}");

    // The created project is the fixture's own scratch project, with the
    // fixture clock's instant readable from its metadata.
    let id = made["project_id"].as_str().unwrap().to_owned();
    let meta = fx.project_json(&id);
    assert_eq!(meta["project_id"], made["project_id"], "{made}");
    assert!(meta["archived"].is_null(), "{meta}");

    let key = made["operator_key"]["key"].as_str().unwrap().to_owned();
    assert!(key.starts_with("vivi_"));
    let (errored, listed) = fx.call_as(&key, "pump_list", json!({"project": id})).await;
    assert!(!errored, "{listed}");
    assert_eq!(listed["total"], 0, "{listed}");

    // The clock helper agrees with the store's fixed instant.
    assert_eq!(fx.clock.load(Ordering::SeqCst), BASE_SECONDS);
    assert_eq!(rfc3339(BASE_SECONDS), "2026-10-14T17:46:40Z");
}
