//! Archive metadata on `project.json`: the [`Archived`] record, its serde
//! defaults and omissions, and `set_archived` reading and writing through
//! retirement while the ordinary load and write paths keep refusing retired
//! ids. Everything runs against a scratch data directory.

use std::collections::HashSet;
use std::path::PathBuf;

use chrono::{DateTime, TimeZone, Utc};
use serde_json::json;

use super::{Archived, ProjectError, ProjectMeta, ProjectStore};

fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("vivi-server-archive-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn record(root: &str) -> Archived {
    Archived {
        at: "2026-10-25T12:00:00+00:00".into(),
        by: "mind".into(),
        root: PathBuf::from(root),
        lanes_dir: PathBuf::from(format!("{root}/lanes")),
        pump: true,
        serve: false,
    }
}

fn assert_project_not_found(err: ProjectError) {
    assert!(matches!(err, ProjectError::ProjectNotFound));
}

/// A `project.json` written before the `archived` field existed parses with
/// `archived: None`.
#[test]
fn old_json_without_archived_parses_with_none() {
    let meta: ProjectMeta = serde_json::from_str(r#"{"project_id":"abcd1234","epoch":7}"#).unwrap();
    assert_eq!(meta.project_id, "abcd1234");
    assert_eq!(meta.epoch, Some(7));
    assert!(meta.archived.is_none());
    assert!(!meta.is_archived());
}

/// A live project's JSON carries no `archived` key at all.
#[test]
fn live_meta_omits_archived_key() {
    let dir = scratch("live-omits");
    let store = ProjectStore::new(&dir);
    let meta = store.create_project().unwrap();
    let text = std::fs::read_to_string(
        store
            .project_dir(&meta.project_id)
            .unwrap()
            .join("project.json"),
    )
    .unwrap();
    assert!(!text.contains("archived"));
}

/// An archive record survives a JSON round trip with its paths and flags.
#[test]
fn archived_round_trips_paths_and_flags() {
    let dir = scratch("round-trip");
    let store = ProjectStore::new(&dir);
    let id = store.create_project().unwrap().project_id;
    let written = store
        .set_archived(&id, Some(record("/tmp/mailspace")))
        .unwrap();
    let text =
        std::fs::read_to_string(store.project_dir(&id).unwrap().join("project.json")).unwrap();

    let read: ProjectMeta = serde_json::from_str(&text).unwrap();
    assert!(read.is_archived());
    let saved = read.archived.as_ref().unwrap();
    let expected = written.archived.as_ref().unwrap();
    assert_eq!(saved.at, expected.at);
    assert_eq!(saved.by, expected.by);
    assert_eq!(saved.root, expected.root);
    assert_eq!(saved.lanes_dir, expected.lanes_dir);
    assert!(saved.pump);
    assert!(!saved.serve);
    assert_eq!(*saved, *expected);
}

/// `pump` and `serve` default to false when an archive record was saved
/// without them.
#[test]
fn archived_flags_default_false() {
    let meta: ProjectMeta = serde_json::from_value(json!({
        "project_id": "abcd1234",
        "archived": {
            "at": "2026-10-25T12:00:00+00:00",
            "by": "mind",
            "root": "/tmp/mailspace",
            "lanes_dir": "/tmp/mailspace/lanes"
        }
    }))
    .unwrap();
    let saved = meta.archived.unwrap();
    assert!(!saved.pump);
    assert!(!saved.serve);
}

/// `set_archived` stamps and clears a retired id's metadata; the ordinary
/// read refuses the id throughout.
#[test]
fn set_and_clear_archived_on_retired_id() {
    let dir = scratch("set-and-clear");
    let store = ProjectStore::new(&dir);
    let id = store.create_project().unwrap().project_id;
    store.set_retired(HashSet::from([id.clone()]));
    assert!(store.is_retired(&id));
    assert_project_not_found(store.load_meta(&id).unwrap_err());

    let written = store
        .set_archived(&id, Some(record("/srv/archive/mailspace")))
        .unwrap();
    assert!(written.is_archived());
    let read = store.load_meta_including_retired(&id).unwrap();
    assert_eq!(read.archived, written.archived);

    let cleared = store.set_archived(&id, None).unwrap();
    assert!(!cleared.is_archived());
    assert!(
        store
            .load_meta_including_retired(&id)
            .unwrap()
            .archived
            .is_none()
    );
}

/// Retirement keeps refusing the ordinary paths: `load_meta` and
/// `write_meta` both answer `ProjectNotFound` for a retired id, archived
/// record or not.
#[test]
fn ordinary_load_and_write_still_refuse_retired() {
    let dir = scratch("ordinary-refuse");
    let store = ProjectStore::new(&dir);
    let meta = store.create_project().unwrap();
    let id = meta.project_id.clone();
    store.set_retired(HashSet::from([id.clone()]));

    assert_project_not_found(store.load_meta(&id).unwrap_err());
    assert_project_not_found(store.write_meta(&meta).unwrap_err());

    store
        .set_archived(&id, Some(record("/srv/archive/mailspace")))
        .unwrap();
    assert_project_not_found(store.load_meta(&id).unwrap_err());
    assert_project_not_found(store.write_meta(&meta).unwrap_err());
}

/// Stamping the archive record preserves the epoch and the rest of the
/// metadata; clearing restores it exactly.
#[test]
fn archived_preserves_epoch_and_metadata() {
    let dir = scratch("preserve");
    let store = ProjectStore::new(&dir);
    let meta = store.create_project().unwrap();
    let id = meta.project_id.clone();
    let epoch = store.epoch(&id).unwrap();
    store.set_retired(HashSet::from([id.clone()]));

    let written = store
        .set_archived(&id, Some(record("/srv/archive/mailspace")))
        .unwrap();
    assert_eq!(written.project_id, id);
    assert_eq!(written.epoch, Some(epoch));
    assert_eq!(written.mode, meta.mode);
    assert_eq!(written.closed, meta.closed);
    assert_eq!(written.mailspace_root, meta.mailspace_root);

    let cleared = store.set_archived(&id, None).unwrap();
    assert_eq!(cleared.project_id, id);
    assert_eq!(cleared.epoch, Some(epoch));
    assert!(cleared.archived.is_none());
}

/// An id with no `project.json` on disk is refused through retirement too.
#[test]
fn set_archived_on_missing_id_is_not_found() {
    let dir = scratch("missing-id");
    let store = ProjectStore::new(&dir);
    let missing = "aaaaa-aaaaa-aaaaa-aaaaa-aaaaa";
    assert_project_not_found(
        store
            .set_archived(missing, Some(record("/srv/archive/mailspace")))
            .unwrap_err(),
    );
    assert_project_not_found(store.load_meta_including_retired(missing).unwrap_err());
}

/// `now()` reads the clock `with_clock` installed.
#[test]
fn now_follows_with_clock() {
    let fixed: DateTime<Utc> = Utc.timestamp_opt(1_792_000_000, 0).unwrap();
    let store = ProjectStore::with_clock(&scratch("clock"), move || fixed);
    assert_eq!(store.now(), fixed);
}
