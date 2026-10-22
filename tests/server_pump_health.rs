//! `GET /v1/projects/{id}/pump/health` and the `pump` MCP tool: a hosted
//! project's pump health, read-only, for any authenticated member.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::util::ServiceExt;
use vivi::mailspace::Mailspace;
use vivi::pump::health::HealthOptions;
use vivi::pump::state::{State, Status, Unit};
use vivi::pump::store::{DB_FILENAME, PumpStore};
use vivi_server::{AppState, app_state, hosted, pump_health, router};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "vivi-server-pump-health-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Fx {
    app: Router,
    state: AppState,
    root: PathBuf,
    project: String,
    member: String,
}

/// A served host root; `pump` appends a `[pump]` table to its config.
fn fixture(name: &str, pump: bool) -> Fx {
    let base = scratch(name);
    let root = base.join("hosts").join("app");
    let mut ms = Mailspace::init(Some(&root)).unwrap();
    ms.ensure_operator_role().unwrap();
    if pump {
        let toml = root.join(".vivi").join("mailspace.toml");
        let mut text = std::fs::read_to_string(&toml).unwrap();
        text.push_str("\n[pump]\ntick = 20\n");
        std::fs::write(&toml, text).unwrap();
    }
    let state = app_state(base.join("data"), None, None);
    let report = hosted::reconcile(
        &state.projects,
        &state.identities,
        &BTreeSet::from([root.clone()]),
    );
    assert!(report.refused.is_empty(), "{report:?}");
    Fx {
        app: router(state.clone()),
        root,
        project: report.served[0].project_id.clone(),
        member: report.served[0].member.clone(),
        state,
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// A pump home with one merged and one stuck unit and a fresh tick.
fn write_pump(root: &Path, at: u64) {
    let home = root.join(".vivi").join("pump");
    std::fs::create_dir_all(&home).unwrap();
    let mut st = State {
        merges: 1,
        ..State::default()
    };
    let mut merged = Unit {
        need: "n1".into(),
        status: Status::Merged,
        since: at - 30,
        ..Unit::default()
    };
    merged.commit = Some("abc1234".into());
    merged.merged_at = Some(at - 30);
    st.units.insert("u1".into(), merged);
    st.units.insert(
        "u2".into(),
        Unit {
            need: "n1".into(),
            status: Status::Stuck,
            since: at - 4000,
            ..Unit::default()
        },
    );
    let mut store = PumpStore::open(&home.join(DB_FILENAME)).unwrap();
    store.save(&st).unwrap();
    store.set_control_mode("running").unwrap();
    drop(store);
    let tick = json!({"id": "t1", "started_at": at - 2, "ended_at": at - 1,
        "duration_secs": 1, "outcome": "ok"});
    std::fs::write(home.join("monitor-cycles.jsonl"), format!("{tick}\n")).unwrap();
    std::fs::write(home.join("status.txt"), "status\n").unwrap();
}

impl Fx {
    fn key(&self) -> String {
        self.state
            .keys
            .mint(&self.project, &self.member, "monitor", None)
            .unwrap()
            .key
    }

    async fn get(&self, query: &str, token: Option<&str>) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method("GET")
            .uri(format!("/v1/projects/{}/pump/health{query}", self.project));
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        let response = self
            .app
            .clone()
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn rpc(&self, token: &str, method: &str, params: Value) -> Value {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/projects/{}/mcp", self.project))
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }
}

#[tokio::test]
async fn health_needs_a_bearer() {
    let fx = fixture("bearer", true);
    let (status, body) = fx.get("", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    let (status, _) = fx.get("", Some("not-a-token")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_hosted_project_without_a_pump_table_is_unknown() {
    let fx = fixture("no-pump", false);
    let (status, body) = fx.get("", Some(&fx.key())).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["verdict"], "unknown");
    assert!(
        body["reasons"][0]
            .as_str()
            .unwrap()
            .contains("no [pump] table"),
        "{body}"
    );
}

#[tokio::test]
async fn a_saved_pump_reads_as_json_with_changes_since_a_cursor() {
    let fx = fixture("ready", true);
    let at = now();
    write_pump(&fx.root, at);
    let since = at - 100;
    let (status, body) = fx.get(&format!("?since={since}"), Some(&fx.key())).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["verdict"], "degraded", "{body}");
    assert!(
        body["reasons"]
            .as_array()
            .unwrap()
            .contains(&json!("1 units stuck")),
        "{body}"
    );
    assert_eq!(body["control"], "running");
    assert_eq!(body["units"]["merged"], 1);
    assert_eq!(body["units"]["stuck"], 1);
    assert_eq!(body["merges"]["recent"][0]["commit"], "abc1234");
    assert_eq!(body["blocked"][0]["id"], "u2");
    assert_eq!(body["tick_secs"], 20);
    assert_eq!(body["stale_after_secs"], 600);
    assert_eq!(body["changed_since"]["since"], since);
    assert_eq!(body["changed_since"]["merges"], 1);
    assert_eq!(body["changed_since"]["units"][0]["unit"], "u1");
    let (_, plain) = fx.get("?stale_after=5", Some(&fx.key())).await;
    assert_eq!(plain["stale_after_secs"], 5);
    assert!(plain.get("changed_since").is_none(), "{plain}");
}

#[tokio::test]
async fn health_writes_nothing_under_the_pump_home() {
    let fx = fixture("read-only", true);
    write_pump(&fx.root, now());
    let home = fx.root.join(".vivi").join("pump");
    let before = listing(&home);
    let (status, _) = fx.get("", Some(&fx.key())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listing(&home), before);
}

fn listing(dir: &Path) -> Vec<(String, u64)> {
    let mut out: Vec<(String, u64)> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let len = entry.metadata().unwrap().len();
            (entry.file_name().to_string_lossy().into_owned(), len)
        })
        .filter(|(name, _)| !name.ends_with("-shm") && !name.ends_with("-wal"))
        .collect();
    out.sort();
    out
}

#[tokio::test]
async fn a_bad_query_is_a_400() {
    let fx = fixture("query", true);
    let (status, body) = fx.get("?since=yesterday", Some(&fx.key())).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, _) = fx.get("?pause=1", Some(&fx.key())).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_pump_mcp_tool_answers_the_same_verdict() {
    let fx = fixture("mcp", true);
    write_pump(&fx.root, now());
    let key = fx.key();
    let listed = fx.rpc(&key, "tools/list", json!({})).await;
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"pump"), "{names:?}");
    let value = fx
        .rpc(
            &key,
            "tools/call",
            json!({"name": "pump", "arguments": {"stale_after": 900}}),
        )
        .await;
    assert_ne!(value["result"]["isError"], true, "{value}");
    let text = value["result"]["content"][0]["text"].as_str().unwrap();
    let body: Value = serde_json::from_str(text).unwrap();
    assert_eq!(body["verdict"], "degraded");
    assert_eq!(body["stale_after_secs"], 900);
    let bad = fx
        .rpc(
            &key,
            "tools/call",
            json!({"name": "pump", "arguments": {"since": "soon"}}),
        )
        .await;
    assert_eq!(bad["result"]["isError"], true, "{bad}");
}

#[test]
fn a_project_that_is_not_hosted_has_no_pump() {
    let fx = fixture("not-hosted", true);
    let answer = pump_health::read(
        &fx.state.projects,
        "not-a-project",
        &HealthOptions::default(),
    );
    assert_eq!(answer, Ok(None));
}
