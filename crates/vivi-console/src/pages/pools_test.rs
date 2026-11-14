use axum::http::StatusCode;

use super::super::browser_test::Browser;
use crate::data::pools::tests::CONFIG;
use crate::host::{Principal, ProjectRef};

fn principal_of(root: &std::path::Path) -> Principal {
    Principal {
        name: "alice".into(),
        projects: vec![ProjectRef {
            id: "proj-1".into(),
            name: "demo".into(),
            mailspace_root: root.to_path_buf(),
        }],
    }
}

#[tokio::test]
async fn the_pools_page_shows_roles_and_pools_and_no_secrets() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".vivi")).unwrap();
    std::fs::write(dir.path().join(".vivi/mailspace.toml"), CONFIG).unwrap();
    let browser = Browser::open(principal_of(dir.path())).await;
    let (status, page) = browser.get("/ui/p/demo/pools").await;
    assert_eq!(status, StatusCode::OK);
    for expected in [
        "mind",
        "hand",
        "model-a",
        "model-b",
        "main",
        "spare",
        "local",
        "5m",
        "claude",
        "build",
        "s1 on studio-01",
    ] {
        assert!(page.contains(expected), "{expected} missing from {page}");
    }
    for planted in [
        "sekret",
        "SEKRET",
        "key_cmd",
        "api_key",
        "auth_token",
        "secret_model",
        "judgment",
    ] {
        assert!(!page.contains(planted), "{planted} leaked into {page}");
    }
    assert!(!page.contains("not built yet"));
}

#[tokio::test]
async fn a_project_whose_config_cannot_be_read_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let browser = Browser::open(principal_of(dir.path())).await;
    let (status, page) = browser.get("/ui/p/demo/pools").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(page.contains("Could not read this project"));
}
