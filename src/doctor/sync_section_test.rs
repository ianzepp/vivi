//! Companion tests for the `sync` doctor section.

use std::fs;

use chrono::{Duration, Utc};

use crate::doctor::Status;
use crate::doctor::sync_section::findings;
use crate::mailspace::Mailspace;
use crate::sync::identity;
use crate::sync::state::SyncState;
use crate::sync::sync_dir;

/// Init a mailspace in a temp dir, then append a `[sync]` table to its config
/// and reopen it so the section reads the real config path.
fn configured(project: Option<&str>, name: Option<&str>) -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let space = Mailspace::init(Some(tmp.path())).expect("init");
    let path = space.config_path();
    let mut text = fs::read_to_string(&path).expect("config");
    text.push_str("\n[sync]\nserver = \"http://127.0.0.1:9\"\n");
    if let Some(project) = project {
        text.push_str("project = \"");
        text.push_str(project);
        text.push_str("\"\n");
    }
    if let Some(name) = name {
        text.push_str("name = \"");
        text.push_str(name);
        text.push_str("\"\n");
    }
    fs::write(&path, text).expect("write config");
    let space = Mailspace::open_root(tmp.path().to_path_buf()).expect("reopen");
    (tmp, space)
}

fn plain() -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let space = Mailspace::init(Some(tmp.path())).expect("init");
    (tmp, space)
}

fn save_state(space: &Mailspace, project: &str) {
    let state = SyncState {
        project: project.to_string(),
        since: 7,
        ..SyncState::default()
    };
    state.save(&sync_dir(&space.dir)).expect("save sync state");
}

fn by_name<'a>(findings: &'a [crate::doctor::Finding], name: &str) -> &'a crate::doctor::Finding {
    findings
        .iter()
        .find(|finding| finding.name == name)
        .unwrap_or_else(|| panic!("no finding named {name}: {findings:?}"))
}

#[test]
fn not_configured_skips() {
    let (_tmp, space) = plain();
    let report = findings(&space, Utc::now(), None);
    assert_eq!(report.len(), 1, "{report:?}");
    assert_eq!(report[0].section, "sync");
    assert_eq!(report[0].status, Status::Skipped);
    assert_eq!(report[0].detail, "sync not configured");
}

#[test]
fn incomplete_config_warns() {
    let (_tmp, space) = configured(None, Some("this-box"));
    let report = findings(&space, Utc::now(), None);
    let config = by_name(&report, "config");
    assert_eq!(config.status, Status::Warn);
    assert!(config.detail.contains("missing project"), "{config:?}");
    assert!(config.fix.is_some(), "{config:?}");
}

#[test]
fn missing_key_warns_and_never_mints() {
    let (_tmp, space) = configured(Some("proj"), Some("this-box"));
    let key = identity::key_path(&space.dir);
    assert!(!key.exists(), "a fresh mailspace has no key yet");
    let report = findings(&space, Utc::now(), None);
    let finding = by_name(&report, "key");
    assert_eq!(finding.status, Status::Warn);
    assert_eq!(finding.detail, "no sync key on disk");
    assert!(
        finding
            .fix
            .as_deref()
            .unwrap_or_default()
            .contains("sync join"),
        "{finding:?}"
    );
    assert!(!key.exists(), "doctor must never mint the sync key");
}

#[test]
fn stale_sync_age_warns() {
    let (_tmp, space) = configured(Some("proj"), Some("this-box"));
    save_state(&space, "proj");
    let later = Utc::now() + Duration::hours(48);
    let report = findings(&space, later, None);
    let finding = by_name(&report, "age");
    assert_eq!(finding.status, Status::Warn);
    assert!(finding.detail.starts_with("last sync "), "{finding:?}");
    assert!(finding.detail.ends_with("h ago"), "{finding:?}");
    assert_eq!(finding.fix.as_deref(), Some("run vivi sync"));
}

#[test]
fn fresh_sync_age_oks() {
    let (_tmp, space) = configured(Some("proj"), Some("this-box"));
    save_state(&space, "proj");
    let finding = by_name(&findings(&space, Utc::now(), None), "age").clone();
    assert_eq!(finding.status, Status::Ok, "{finding:?}");
    assert!(finding.detail.starts_with("last sync "), "{finding:?}");
}

#[test]
fn never_synced_skips_age() {
    let (_tmp, space) = configured(Some("proj"), Some("this-box"));
    let finding = by_name(&findings(&space, Utc::now(), None), "age").clone();
    assert_eq!(finding.status, Status::Skipped);
    assert_eq!(finding.detail, "never synced");
}

#[test]
fn unreachable_fails() {
    let (_tmp, space) = configured(Some("proj"), Some("this-box"));
    let finding = by_name(&findings(&space, Utc::now(), Some(false)), "server").clone();
    assert_eq!(finding.status, Status::Fail);
    assert_eq!(finding.detail, "server unreachable");
    assert!(finding.fix.is_some(), "{finding:?}");
}

#[test]
fn reachable_ok() {
    let (_tmp, space) = configured(Some("proj"), Some("this-box"));
    let finding = by_name(&findings(&space, Utc::now(), Some(true)), "server").clone();
    assert_eq!(finding.status, Status::Ok);
    assert_eq!(finding.detail, "server reachable");
}

#[test]
fn unprobed_server_skips() {
    let (_tmp, space) = configured(Some("proj"), Some("this-box"));
    let finding = by_name(&findings(&space, Utc::now(), None), "server").clone();
    assert_eq!(finding.status, Status::Skipped);
    assert_eq!(finding.detail, "server reachability not probed");
}
