//! Tests for [`crate::mind::projects`].

use std::collections::BTreeSet;
use std::path::PathBuf;

use tempfile::tempdir;

use super::*;

#[test]
fn watched_is_pumped_roots_less_ignored() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("vivi").join("pump-service.toml");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(
        &file,
        "[[project]]\nroot = \"/w/a\"\npump = true\n\n\
         [[project]]\nroot = \"/w/b\"\npump = false\nserve = true\n\n\
         [[project]]\nroot = \"/w/c\"\n",
    )
    .unwrap();
    let ignored: BTreeSet<PathBuf> = [PathBuf::from("/w/c")].into();
    assert_eq!(
        watched_at(&file, &ignored).unwrap(),
        vec![PathBuf::from("/w/a")]
    );
    let all = list_at(&file, &ignored).unwrap();
    assert_eq!(all.len(), 3);
    assert!(all[2].ignored && all[2].pumped);
    assert!(!all[1].pumped);
}

#[test]
fn missing_registry_watches_nothing() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("pump-service.toml");
    assert!(watched_at(&file, &BTreeSet::new()).unwrap().is_empty());
}

#[test]
fn invalid_registry_is_an_error() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("pump-service.toml");
    std::fs::write(&file, "nonsense = [").unwrap();
    assert!(watched_at(&file, &BTreeSet::new()).is_err());
}
