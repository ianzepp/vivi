use super::*;
use crate::mind::actions::DryRun;

const STATUS: &str = "active=vivi-edge-1a2b3c4d5e6f\nedge=1a2b3c4d5e6f\nprevious=vivi-22.0.0\nrelease=vivi-22.0.0\nswitched_at=1000\n";

fn rollback(build: &str) -> ActionRequest {
    ActionRequest {
        kind: Action::RollbackEdge,
        project: None,
        need: None,
        step: None,
        word: None,
        note: None,
        build: Some(build.into()),
    }
}

fn actuator(dir: &Path, status: &str) -> EdgeActuator {
    let status_path = dir.join("status");
    fs::write(&status_path, status).unwrap();
    EdgeActuator {
        inner: Box::new(DryRun::default()),
        edge: EdgeConfig {
            status: status_path,
            request: dir.join(REQUEST_FILE),
        },
    }
}

#[test]
fn parses_the_installer_status() {
    let status = parse(STATUS);
    assert_eq!(status.active, "vivi-edge-1a2b3c4d5e6f");
    assert_eq!(status.edge.as_deref(), Some("1a2b3c4d5e6f"));
    assert_eq!(status.previous.as_deref(), Some("vivi-22.0.0"));
    assert_eq!(status.switched_at, Some(1000));
}

#[test]
fn a_release_binary_has_no_edge_and_no_watch() {
    let status = parse("active=vivi-22.0.0\nedge=\nswitched_at=1000\n");
    assert_eq!(status.edge, None);
    assert!(!status.watching(1001));
    assert_eq!(status.fingerprints(1000 + 600), Vec::<String>::new());
}

#[test]
fn a_malformed_edge_commit_is_dropped() {
    assert_eq!(parse("edge=1A2B3C4D5E6F\n").edge, None);
    assert_eq!(parse("edge=1a2b; rm -rf\n").edge, None);
}

#[test]
fn checkpoints_appear_in_turn_and_end_with_the_watch() {
    let status = parse(STATUS);
    assert_eq!(status.fingerprints(1000 + 60), Vec::<String>::new());
    assert_eq!(status.fingerprints(1000 + 300), ["edge 1a2b3c4d5e6f +5m"]);
    assert_eq!(
        status.fingerprints(1000 + 1200),
        ["edge 1a2b3c4d5e6f +5m", "edge 1a2b3c4d5e6f +20m"]
    );
    assert_eq!(status.fingerprints(1000 + WATCH_SECS), Vec::<String>::new());
}

#[test]
fn rollback_of_the_active_build_writes_its_commit() {
    let dir = tempfile::tempdir().unwrap();
    let mut actuator = actuator(dir.path(), STATUS);
    let receipt = actuator.apply(&rollback("1a2b3c4d5e6f")).unwrap();
    assert!(receipt.contains("1a2b3c4d5e6f"), "{receipt}");
    let written = fs::read_to_string(dir.path().join(REQUEST_FILE)).unwrap();
    assert_eq!(written, "1a2b3c4d5e6f\n");
}

#[test]
fn rollback_of_another_build_is_refused_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut actuator = actuator(dir.path(), STATUS);
    let error = actuator.apply(&rollback("ffffffffffff")).unwrap_err();
    assert!(error.contains("not active"), "{error}");
    assert!(!dir.path().join(REQUEST_FILE).exists());
}

#[test]
fn other_actions_go_to_the_inner_actuator() {
    let dir = tempfile::tempdir().unwrap();
    let mut actuator = actuator(dir.path(), STATUS);
    let request = ActionRequest {
        kind: Action::PausePump,
        project: Some("/w/app".into()),
        build: None,
        ..rollback("1a2b3c4d5e6f")
    };
    let line = actuator.apply(&request).unwrap();
    assert!(line.starts_with("would run vivi pump pause"), "{line}");
}
