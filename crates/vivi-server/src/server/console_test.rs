use std::path::Path;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use super::*;
use crate::server::grants::{GrantLevel, GrantRecord};
use crate::server::ids::mint_room_id;
use crate::server::{app_state, router};

fn state(dir: &Path) -> AppState {
    app_state(dir.to_path_buf(), None, None).unwrap()
}

/// Write a `project.json` for a new project id and return the id.
fn project(state: &AppState, json: impl FnOnce(&str) -> String) -> String {
    let id = mint_room_id().unwrap();
    let dir = state.projects.project_dir(&id).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("project.json"), json(&id)).unwrap();
    id
}

fn hosted_json(id: &str, root: &Path) -> String {
    format!(
        "{{\"project_id\":\"{id}\",\"mailspace_root\":{}}}",
        serde_json::to_string(root).unwrap()
    )
}

fn grant(id: &str) -> GrantRecord {
    GrantRecord {
        project_id: id.to_string(),
        member: "alice".into(),
        identity: None,
        level: GrantLevel::Member,
    }
}

fn mint(state: &AppState, principal: &str, host: bool, grants: Vec<GrantRecord>) -> String {
    state
        .server_keys
        .mint(principal, host, grants, "console", "alice")
        .unwrap()
        .key
}

#[test]
fn a_grant_on_a_hosted_project_becomes_a_project_ref() {
    let data = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let state = state(data.path());
    let id = project(&state, |id| hosted_json(id, root.path()));
    let key = mint(&state, "alice", false, vec![grant(&id)]);

    let host = ServerConsoleHost::new(state);
    let principal = host.authenticate(&key).unwrap();
    assert_eq!(principal.name, "alice");
    assert_eq!(principal.projects.len(), 1);
    let project = &principal.projects[0];
    assert_eq!(project.id, id);
    assert_eq!(project.mailspace_root, root.path());
    assert_eq!(
        project.name,
        root.path().file_name().unwrap().to_string_lossy()
    );
}

#[test]
fn only_readable_hosted_projects_are_listed() {
    let data = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let state = state(data.path());
    let hosted = project(&state, |id| hosted_json(id, root.path()));
    let durable = project(&state, |id| format!("{{\"project_id\":\"{id}\"}}"));
    let archived = project(&state, |id| {
        format!(
            "{{\"project_id\":\"{id}\",\"mailspace_root\":{},\"archived\":{{\"at\":\"2026-01-01T00:00:00Z\",\"by\":\"op\",\"root\":\"/x\",\"lanes_dir\":\"/y\"}}}}",
            serde_json::to_string(root.path()).unwrap()
        )
    });
    let missing = mint_room_id().unwrap();
    let key = mint(
        &state,
        "alice",
        false,
        vec![
            grant(&hosted),
            grant(&durable),
            grant(&archived),
            grant(&missing),
        ],
    );
    let host = ServerConsoleHost::new(state);
    let principal = host.authenticate(&key).unwrap();
    let ids: Vec<&str> = principal.projects.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, [hosted.as_str()]);
}

#[test]
fn unknown_keys_do_not_authenticate_and_hosts_see_no_projects() {
    let data = tempfile::tempdir().unwrap();
    let state = state(data.path());
    let host_key = mint(&state, "host", true, Vec::new());
    let host = ServerConsoleHost::new(state);
    assert!(host.authenticate("vivi_nope").is_none());
    assert!(host.authenticate("").is_none());
    let principal = host.authenticate(&host_key).unwrap();
    assert!(principal.projects.is_empty());
}

#[tokio::test]
async fn the_server_router_serves_the_console_under_ui() {
    let data = tempfile::tempdir().unwrap();
    let app = router(state(data.path()));
    let request = Request::builder()
        .uri("/ui/login")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let request = Request::builder().uri("/ui/").body(Body::empty()).unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
}
