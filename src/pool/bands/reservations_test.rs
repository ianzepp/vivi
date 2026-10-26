//! Companion tests for `reservations`: live-claim scanning, guard cleanup,
//! abandoned unlocked files, path-safe identities and admission-busy.

use std::fs;
use std::fs::OpenOptions;

use super::*;

fn row(pool: &str, job: &str, model: &str) -> ReservationRow {
    ReservationRow {
        pool: pool.to_string(),
        job: job.to_string(),
        model: model.to_string(),
    }
}

#[test]
fn reservations_scan_live_until_their_guard_drops() {
    let dir = tempfile::tempdir().unwrap();
    let held = admission_lock(dir.path(), "gpu").unwrap();
    let first = reserve(dir.path(), row("gpu", "job-1", "zai/glm-5.3-flash")).unwrap();
    let second = reserve(dir.path(), row("gpu", "job-2", "zai/glm-5.3-flash")).unwrap();

    let live = scan_live(dir.path(), "gpu").unwrap();
    assert_eq!(live.len(), 2, "both in-flight claims are live");
    assert!(live.contains(&row("gpu", "job-1", "zai/glm-5.3-flash")));

    drop(first);
    assert_eq!(
        scan_live(dir.path(), "gpu").unwrap(),
        vec![row("gpu", "job-2", "zai/glm-5.3-flash")]
    );

    drop(second);
    drop(held);
    assert!(scan_live(dir.path(), "gpu").unwrap().is_empty());
}

#[test]
fn a_pool_without_reservations_scans_empty() {
    let dir = tempfile::tempdir().unwrap();
    let held = admission_lock(dir.path(), "gpu").unwrap();
    assert!(scan_live(dir.path(), "gpu").unwrap().is_empty());
    drop(held);
}

#[test]
fn dropping_a_reservation_removes_its_file_and_releases_its_lock() {
    let dir = tempfile::tempdir().unwrap();
    let guard = reserve(dir.path(), row("gpu", "job-1", "m")).unwrap();
    let path = reservation_path(dir.path(), "gpu", "job-1");
    assert!(path.exists(), "the reservation file exists while held");
    let text = fs::read_to_string(&path).unwrap();
    assert_eq!(
        serde_json::from_str::<ReservationRow>(&text).unwrap(),
        row("gpu", "job-1", "m"),
        "the file holds the immutable row"
    );
    assert_eq!(guard.row(), &row("gpu", "job-1", "m"));

    drop(guard);
    assert!(!path.exists(), "drop removes the reservation file");
    reserve(dir.path(), row("gpu", "job-1", "m")).unwrap();
}

#[test]
fn an_abandoned_unlocked_file_is_reclaimed_not_counted() {
    let dir = tempfile::tempdir().unwrap();
    let abandoned = reservation_path(dir.path(), "gpu", "dead-job");
    fs::create_dir_all(abandoned.parent().unwrap()).unwrap();
    fs::write(
        &abandoned,
        serde_json::to_string(&row("gpu", "dead-job", "m")).unwrap(),
    )
    .unwrap();

    assert!(scan_live(dir.path(), "gpu").unwrap().is_empty());
    assert!(!abandoned.exists(), "the abandoned file is reclaimed");
}

#[test]
fn a_locked_file_stays_live_whatever_its_age() {
    let dir = tempfile::tempdir().unwrap();
    let path = reservation_path(dir.path(), "gpu", "slow-job");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        serde_json::to_string(&row("gpu", "slow-job", "m")).unwrap(),
    )
    .unwrap();
    // A separate handle conflicts like another dispatch's process would.
    let held = OpenOptions::new().write(true).open(&path).unwrap();
    held.try_lock().unwrap();

    assert_eq!(
        scan_live(dir.path(), "gpu").unwrap(),
        vec![row("gpu", "slow-job", "m")],
        "a live claim counts whatever its age"
    );
    assert!(path.exists(), "a live claim is never removed by a scan");
}

#[test]
fn an_unreadable_live_claim_fails_the_scan_instead_of_skipping_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = reservation_path(dir.path(), "gpu", "broken-job");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "not a reservation row").unwrap();
    let held = OpenOptions::new().write(true).open(&path).unwrap();
    held.try_lock().unwrap();

    let error = scan_live(dir.path(), "gpu").unwrap_err().to_string();
    assert!(error.contains("reservation row"), "{error}");
    assert!(path.exists(), "the unreadable claim is left for its holder");
}

#[test]
fn the_admission_lock_reports_busy_and_pools_are_independent() {
    let dir = tempfile::tempdir().unwrap();
    let held = admission_lock(dir.path(), "gpu").unwrap();

    let busy = admission_lock(dir.path(), "gpu").unwrap_err();
    assert!(matches!(busy, AdmissionLockError::Busy), "{busy}");
    assert!(busy.to_string().contains("admission-busy"), "{busy}");
    assert!(
        admission_lock(dir.path(), "other").is_ok(),
        "other pools are free"
    );

    drop(held);
    admission_lock(dir.path(), "gpu").unwrap();
}

#[test]
fn identities_become_path_safe_names_inside_the_admission_dir() {
    let dir = tempfile::tempdir().unwrap();
    let pool = "a/b c";
    let job = "job/../x y";
    let held = admission_lock(dir.path(), pool).unwrap();
    let reservation = reserve(dir.path(), row(pool, job, "m")).unwrap();
    drop(held);

    let admission = dir.path().join(ADMISSION_DIR);
    let file = reservation_path(dir.path(), pool, job);
    assert!(
        file.starts_with(&admission),
        "{} escapes the admission dir",
        file.display()
    );
    assert!(file.is_file(), "reserved: {}", file.display());
    for part in file.strip_prefix(&admission).unwrap().components() {
        let name = part.as_os_str().to_string_lossy();
        let stem = name.trim_end_matches(".json").trim_end_matches(".lock");
        assert!(
            name == "reservations"
                || (!stem.is_empty() && stem.chars().all(|c| c.is_ascii_hexdigit())),
            "{name} is not a path-safe hashed name"
        );
    }
    let lock_name = lock_path(dir.path(), pool)
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let lock_stem = lock_name.trim_end_matches(".lock");
    assert!(!lock_stem.is_empty() && lock_stem.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(scan_live(dir.path(), pool).unwrap().len(), 1);
    drop(reservation);
    assert!(
        !file.exists(),
        "drop removes the reservation even for odd identities"
    );
}

#[test]
fn identity_hashes_are_stable_and_separated() {
    assert_eq!(identity_hash("gpu-1"), identity_hash("gpu-1"));
    assert_ne!(identity_hash("gpu-1"), identity_hash("gpu-11"));
    assert_ne!(identity_hash("gpu-1"), identity_hash("gpu/1"));
}
