use super::*;

#[test]
fn require_lanes_needs_only_the_lanes_root() {
    let configured = HostConfig {
        lanes_root: Some(PathBuf::from("/lanes")),
        ..HostConfig::default()
    };
    let lanes = require_lanes(&configured, "archive_project")
        .map_err(|error| error.message)
        .unwrap();
    assert_eq!(lanes, PathBuf::from("/lanes"));
    let error = require_lanes(&HostConfig::default(), "archive_project")
        .err()
        .expect("an unset HOST_LANES_ROOT is an error")
        .message;
    assert_eq!(
        error,
        "archive_project is not configured on this host: HOST_LANES_ROOT not set"
    );
}
