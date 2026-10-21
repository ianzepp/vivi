//! Tests for [`crate::registry`].

use std::collections::BTreeSet;
use std::path::PathBuf;

use tempfile::tempdir;

use super::{Flags, Registry, fingerprint_at, load_at, save_at};

fn p(path: &str) -> PathBuf {
    PathBuf::from(path)
}

fn set(paths: &[&str]) -> BTreeSet<PathBuf> {
    paths.iter().map(|path| p(path)).collect()
}

const PUMP_ONLY: Flags = Flags {
    pump: true,
    serve: false,
};
const SERVE_ONLY: Flags = Flags {
    pump: false,
    serve: true,
};
const BOTH: Flags = Flags {
    pump: true,
    serve: true,
};

/// The old `projects = [...]` list reads as pump on, serve off.
#[test]
fn legacy_list_reads_as_pumped_not_served() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("pump-service.toml");
    std::fs::write(&path, "projects = [\"/tmp/b\", \"/tmp/a\"]\n").unwrap();
    let registry = load_at(&path).unwrap();
    assert_eq!(registry.pumped(), set(&["/tmp/a", "/tmp/b"]));
    assert!(registry.served().is_empty());
    assert_eq!(registry.get(&p("/tmp/a")), Some(PUMP_ONLY));
}

/// A write migrates the legacy file to the table format and keeps the roots.
#[test]
fn write_migrates_legacy_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("pump-service.toml");
    std::fs::write(&path, "projects = [\"/tmp/a\"]\n").unwrap();
    let registry = load_at(&path).unwrap();
    save_at(&path, &registry).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("[[project]]"), "{text}");
    assert!(!text.contains("projects"), "{text}");
    assert_eq!(load_at(&path).unwrap(), registry);
}

/// The table format round-trips and omitted flags default to pump on, serve off.
#[test]
fn table_format_round_trips() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("pump-service.toml");
    let mut registry = Registry::default();
    registry.set(p("/tmp/a"), BOTH);
    registry.set(p("/tmp/b"), SERVE_ONLY);
    registry.set(p("/tmp/c"), PUMP_ONLY);
    save_at(&path, &registry).unwrap();
    assert_eq!(load_at(&path).unwrap(), registry);

    std::fs::write(&path, "[[project]]\nroot = \"/tmp/d\"\n").unwrap();
    assert_eq!(load_at(&path).unwrap().get(&p("/tmp/d")), Some(PUMP_ONLY));
}

/// A missing file is an empty registry, not an error.
#[test]
fn missing_file_is_empty() {
    let dir = tempdir().unwrap();
    let registry = load_at(&dir.path().join("absent.toml")).unwrap();
    assert!(registry.entries().is_empty());
}

/// Unknown keys are rejected, at the top level and inside an entry, and the
/// error names the file.
#[test]
fn unknown_fields_are_rejected() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("pump-service.toml");
    std::fs::write(&path, "wat = 1\n").unwrap();
    let error = load_at(&path).unwrap_err().to_string();
    assert!(error.contains(&path.display().to_string()), "{error}");

    std::fs::write(&path, "[[project]]\nroot = \"/tmp/a\"\nwat = 1\n").unwrap();
    assert!(load_at(&path).is_err());
}

/// `add` registers once and leaves an existing entry alone; `set` replaces
/// flags and reports whether anything changed; `remove` reports presence.
#[test]
fn add_set_remove() {
    let mut registry = Registry::default();
    assert!(registry.add(p("/tmp/a"), PUMP_ONLY));
    assert!(!registry.add(p("/tmp/a"), SERVE_ONLY));
    assert_eq!(registry.get(&p("/tmp/a")), Some(PUMP_ONLY));

    assert!(registry.set(p("/tmp/a"), BOTH));
    assert!(!registry.set(p("/tmp/a"), BOTH));
    assert!(registry.set(p("/tmp/b"), SERVE_ONLY));

    assert!(registry.remove(&p("/tmp/a")));
    assert!(!registry.remove(&p("/tmp/a")));
    assert_eq!(registry.roots(), set(&["/tmp/b"]));
}

/// `pumped` and `served` filter on their own flag only.
#[test]
fn pumped_and_served_filter_by_flag() {
    let mut registry = Registry::default();
    registry.set(p("/tmp/pump"), PUMP_ONLY);
    registry.set(p("/tmp/serve"), SERVE_ONLY);
    registry.set(p("/tmp/both"), BOTH);
    registry.set(
        p("/tmp/none"),
        Flags {
            pump: false,
            serve: false,
        },
    );
    assert_eq!(registry.pumped(), set(&["/tmp/both", "/tmp/pump"]));
    assert_eq!(registry.served(), set(&["/tmp/both", "/tmp/serve"]));
    assert_eq!(registry.roots().len(), 4);
    assert_eq!(registry.entries().len(), 4);
}

/// The fingerprint changes when content changes and is stable otherwise.
#[test]
fn fingerprint_tracks_content() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("pump-service.toml");
    let absent = fingerprint_at(&path).unwrap();
    assert_eq!(absent, fingerprint_at(&path).unwrap());

    let mut registry = Registry::default();
    registry.set(p("/tmp/a"), PUMP_ONLY);
    save_at(&path, &registry).unwrap();
    let first = fingerprint_at(&path).unwrap();
    assert_ne!(first, absent);
    assert_eq!(first, fingerprint_at(&path).unwrap());

    registry.set(p("/tmp/a"), BOTH);
    save_at(&path, &registry).unwrap();
    assert_ne!(first, fingerprint_at(&path).unwrap());
}
