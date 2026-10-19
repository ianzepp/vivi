use std::path::Path;

use tempfile::tempdir;

use super::{LocalSite, resolve};
use crate::error::VivariumError;

#[test]
fn relative_root_resolves_under_project() {
    let project = tempdir().expect("temp project");
    let site = resolve("pool/lanes", project.path(), "ms", "here").expect("resolve");
    assert_eq!(
        site,
        LocalSite {
            root: project.path().join("pool/lanes"),
            pool_dir: project.path().join("pool/lanes/here"),
            relative: true,
            mailspace: "ms".to_string(),
            pool: "here".to_string(),
        }
    );
}

#[test]
fn absolute_root_keeps_the_mailspace_segment() {
    let project = tempdir().expect("temp project");
    let site = resolve("/srv", project.path(), "ms", "here").expect("resolve");
    assert_eq!(site.root, Path::new("/srv"));
    assert_eq!(site.pool_dir, Path::new("/srv/ms/here"));
    assert!(!site.relative);
}

#[test]
fn home_root_expands() {
    let project = tempdir().expect("temp project");
    let home = dirs::home_dir().expect("home dir");
    let site = resolve("~/x", project.path(), "ms", "here").expect("resolve");
    assert_eq!(site.root.to_string_lossy(), home.join("x").to_string_lossy());
    assert_eq!(
        site.pool_dir.to_string_lossy(),
        home.join("x/ms/here").to_string_lossy()
    );
    assert!(!site.relative);
}

#[test]
fn rejects_dotdot_vivi_and_project() {
    let project = tempdir().expect("temp project");
    for root in ["..", ".vivi", "", "."] {
        let err = resolve(root, project.path(), "ms", "here").expect_err(root);
        assert!(matches!(err, VivariumError::Config(_)), "{root}: {err:?}");
    }
}
