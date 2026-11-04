use std::fs;
use std::time::{Duration, SystemTime};

use super::{RefreshClass, classify_status, is_dead, mark_dead};

#[test]
fn classify_status_splits_refused_from_transient() {
    assert_eq!(classify_status(reqwest::StatusCode::OK), RefreshClass::Ok);
    assert_eq!(
        classify_status(reqwest::StatusCode::BAD_REQUEST),
        RefreshClass::Refused
    );
    assert_eq!(
        classify_status(reqwest::StatusCode::UNAUTHORIZED),
        RefreshClass::Refused
    );
    assert_eq!(
        classify_status(reqwest::StatusCode::TOO_MANY_REQUESTS),
        RefreshClass::Transient
    );
    assert_eq!(
        classify_status(reqwest::StatusCode::INTERNAL_SERVER_ERROR),
        RefreshClass::Transient
    );
}

#[test]
fn mark_dead_sticks_until_the_file_identity_changes() {
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("creds.json");
    fs::write(&path, r#"{"access_token":"old"}"#).expect("write");
    assert!(!is_dead(&path));
    mark_dead(&path);
    assert!(is_dead(&path));
    mark_dead(&path);
    assert!(is_dead(&path));
    fs::write(&path, r#"{"access_token":"fresh-from-login"}"#).expect("rewrite");
    assert!(!is_dead(&path));
}

#[test]
fn touching_mtime_clears_dead() {
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("creds.json");
    fs::write(&path, r#"{"access_token":"old"}"#).expect("write");
    mark_dead(&path);
    assert!(is_dead(&path));
    let later = SystemTime::now() + Duration::from_secs(5);
    let file = fs::File::options().write(true).open(&path).expect("open");
    file.set_modified(later).expect("mtime");
    assert!(!is_dead(&path));
}
