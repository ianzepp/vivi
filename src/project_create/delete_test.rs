//! Tests for [`super`] (`project_create::delete`).

use std::fs::{self, File};
use std::path::PathBuf;

use tempfile::{TempDir, tempdir};

use super::{ProjectDelete, delete_at};
use crate::project_create::{ProjectCreate, create_at};
use crate::registry;

struct Site {
    _tmp: TempDir,
    projects: PathBuf,
    lanes: PathBuf,
    registry: PathBuf,
}

impl Site {
    fn new() -> Self {
        let tmp = tempdir().unwrap();
        let projects = tmp.path().join("projects");
        let lanes = tmp.path().join("lanes");
        fs::create_dir(&projects).unwrap();
        fs::create_dir(&lanes).unwrap();
        let registry = tmp.path().join("config/vivi/pump-service.toml");
        let (projects, lanes) = (
            projects.canonicalize().unwrap(),
            lanes.canonicalize().unwrap(),
        );
        Self {
            _tmp: tmp,
            projects,
            lanes,
            registry,
        }
    }

    fn create(&self, name: &str) {
        let request = ProjectCreate::new(name, &self.projects, &self.lanes, "git@hub:");
        create_at(&request, &self.registry).unwrap();
    }

    fn request(&self, name: &str) -> ProjectDelete {
        ProjectDelete {
            name: name.into(),
            projects_root: self.projects.clone(),
            lanes_root: self.lanes.clone(),
            confirm: name.into(),
        }
    }

    fn registered(&self, name: &str) -> bool {
        registry::load_at(&self.registry)
            .unwrap()
            .get(&self.projects.join(name))
            .is_some()
    }
}

#[test]
fn removes_the_registration_the_root_and_the_lanes() {
    let site = Site::new();
    site.create("app");
    site.create("other");
    let deleted = delete_at(&site.request("app"), &site.registry).unwrap();
    assert!(deleted.unregistered);
    assert_eq!(deleted.project_root, Some(site.projects.join("app")));
    assert_eq!(deleted.lanes_dir, Some(site.lanes.join("app")));
    assert!(!site.projects.join("app").exists());
    assert!(!site.lanes.join("app").exists());
    assert!(!site.registered("app"));
    assert!(site.registered("other") && site.lanes.join("other/other.git").is_dir());
}

#[test]
fn the_confirmation_must_repeat_the_name() {
    let site = Site::new();
    site.create("app");
    let mut request = site.request("app");
    request.confirm = "ap".into();
    let error = delete_at(&request, &site.registry).unwrap_err().to_string();
    assert!(error.contains("repeats its name"), "{error}");
    assert!(site.registered("app") && site.projects.join("app").is_dir());
}

#[test]
fn an_unknown_project_is_an_error() {
    let site = Site::new();
    let error = delete_at(&site.request("ghost"), &site.registry)
        .unwrap_err()
        .to_string();
    assert!(error.contains("no project \"ghost\""), "{error}");
}

#[test]
fn a_live_pump_keeps_the_files_until_a_second_call() {
    let site = Site::new();
    site.create("app");
    let home = site.projects.join("app/.vivi/pump");
    fs::create_dir_all(&home).unwrap();
    let lock = File::create(home.join("lock")).unwrap();
    lock.try_lock().unwrap();

    let error = delete_at(&site.request("app"), &site.registry)
        .unwrap_err()
        .to_string();
    assert!(error.contains("still holds its lock"), "{error}");
    assert!(!site.registered("app"));
    assert!(site.projects.join("app").is_dir() && site.lanes.join("app").is_dir());

    drop(lock);
    let deleted = delete_at(&site.request("app"), &site.registry).unwrap();
    assert!(!deleted.unregistered);
    assert!(deleted.project_root.is_some() && deleted.lanes_dir.is_some());
    assert!(!site.projects.join("app").exists() && !site.lanes.join("app").exists());
}

#[test]
fn a_half_deleted_project_finishes_on_the_next_call() {
    let site = Site::new();
    site.create("app");
    fs::remove_dir_all(site.projects.join("app")).unwrap();
    let deleted = delete_at(&site.request("app"), &site.registry).unwrap();
    assert!(deleted.unregistered);
    assert!(deleted.project_root.is_none());
    assert_eq!(deleted.lanes_dir, Some(site.lanes.join("app")));
}
