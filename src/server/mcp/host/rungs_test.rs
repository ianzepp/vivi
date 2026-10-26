use serde_json::json;

use super::*;

#[test]
fn a_failed_write_after_creation_names_the_partial_create() {
    let gone = std::env::temp_dir().join(format!("vivi-server-no-such-{}", std::process::id()));
    let mut created = ProjectCreated {
        project_root: gone,
        lanes_dir: std::path::PathBuf::new(),
        target: String::new(),
        git_url: String::new(),
        repo_path: std::path::PathBuf::new(),
        checkout_path: std::path::PathBuf::new(),
        mailspace_id: String::new(),
        initial_commit: None,
        imported: None,
        readiness: Readiness {
            ready: true,
            missing: Vec::new(),
        },
    };
    let Ok(rungs) = Rungs::parse(&json!({"rungs": {"need": "make need"}})) else {
        panic!("rungs must parse");
    };
    let error = rungs.seed(&mut created).unwrap_err();
    assert!(error.contains("project was created"), "{error}");
    assert!(error.contains("need:"), "{error}");
    assert!(error.contains("rolled back"), "{error}");
}
