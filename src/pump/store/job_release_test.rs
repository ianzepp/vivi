//! Tests for the `job_release` table methods in [`super`].

use tempfile::tempdir;

use crate::pump::store::{DB_FILENAME, PumpStore};

#[test]
fn a_release_request_is_taken_once() {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    assert!(store.take_job_releases().expect("empty").is_empty());
    store.request_job_release("m1", 5).expect("request");
    store.request_job_release("m1", 6).expect("repeat");
    store.request_job_release("a0", 7).expect("second");
    assert_eq!(store.take_job_releases().expect("take"), ["m1", "a0"]);
    assert!(store.take_job_releases().expect("again").is_empty());
}
