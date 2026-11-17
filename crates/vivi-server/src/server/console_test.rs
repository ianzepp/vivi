use std::path::Path;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use super::*;
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

#[test]
fn a_hosted_project_is_listed_by_its_root_name() {
    let data = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let state = state(data.path());
    let id = project(&state, |id| hosted_json(id, root.path()));

    let projects = ServerConsoleHost::new(state).projects();
    assert_eq!(projects.len(), 1);
    let project = &projects[0];
    assert_eq!(project.id, id);
    assert_eq!(project.mailspace_root, root.path());
    assert_eq!(
        project.name,
        root.path().file_name().unwrap().to_string_lossy()
    );
}

#[test]
fn only_live_hosted_projects_are_listed() {
    let data = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let state = state(data.path());
    let hosted = project(&state, |id| hosted_json(id, root.path()));
    project(&state, |id| format!("{{\"project_id\":\"{id}\"}}"));
    project(&state, |id| {
        format!(
            "{{\"project_id\":\"{id}\",\"mailspace_root\":{},\"archived\":{{\"at\":\"2026-01-01T00:00:00Z\",\"by\":\"op\",\"root\":\"/x\",\"lanes_dir\":\"/y\"}}}}",
            serde_json::to_string(root.path()).unwrap()
        )
    });
    let projects = ServerConsoleHost::new(state).projects();
    let ids: Vec<&str> = projects.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, [hosted.as_str()]);
}

#[tokio::test]
async fn the_server_router_serves_the_console_under_ui_without_sign_in() {
    let data = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let state = state(data.path());
    project(&state, |id| hosted_json(id, root.path()));
    let app = router(state);
    let request = Request::builder().uri("/ui/").body(Body::empty()).unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let request = Request::builder()
        .uri("/ui/login")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
}
