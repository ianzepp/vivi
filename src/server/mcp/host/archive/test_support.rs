//! Direct-handler test fixtures for the host archive lifecycle
//! (`cfg(test)` only, so nothing here reaches a production build).
//!
//! The harness follows the isolated `VIVI_CONFIG_DIR` + scratch
//! `HostConfig` pattern of `tests/host_archive.rs`: one config directory
//! per test binary (the host registry is process-global) and a scratch
//! data/projects/lanes tree per fixture. Every fixture creates its project
//! under a unique safe name through the real parent
//! [`create_project`](super::super::create_project) handler
//! (`initial_commit: true`, `serve: true`), so the handler, the registry
//! and the minted operator key all run for real.
//!
//! The fixture's `ProjectStore` reads a fixed whole-second Unix time from
//! `Fixture::clock` (`Arc<AtomicI64>`) through `ProjectStore::with_clock`.
//! [`Fixture::seed_archived`] stands in for the archive handler (its own
//! slice): it stamps the archive record with the fixture clock, unregisters
//! the project root and reconciles, holding `CREATING` for its own
//! mutation. Restore and purge tests can therefore run independently of the
//! archive handler implementation; no archive timestamp is altered directly
//! and no environment variable delays anything.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Once;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use chrono::{SecondsFormat, TimeZone, Utc};
use serde_json::json;

use super::super::create_project;
use crate::server::AppState;
use crate::server::projects::{Archived, ProjectStore};

const GIT_URL_PREFIX: &str = "git@hub.example.ts.net:";

/// The fixed whole-second Unix instant every fixture starts at, far from
/// any real now.
pub(super) const BASE_SECONDS: i64 = 1_792_000_000;

static CONFIG_DIR: Once = Once::new();

/// Serializes every test of this binary that builds a fixture, for the
/// test's whole life. The host registry is process-global
/// (`VIVI_CONFIG_DIR`) and `reconcile` serves every registry-served root
/// into whatever data dir it is handed, so a sibling registering a project
/// mid-test can import it into this fixture's data dir: distinct names do
/// not isolate data-dir assertions. The guard lives in the fixture, so it is
/// released only after the fixture (and its cleanup) is dropped; a test that
/// panics does not poison it for the rest.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

static NAMES: AtomicU64 = AtomicU64::new(0);

/// One scratch config directory for the whole test binary: the host
/// registry is process-global (`VIVI_CONFIG_DIR`). Every test uses its own
/// names.
fn isolate_registry() {
    CONFIG_DIR.call_once(|| {
        let dir = std::env::temp_dir().join(format!(
            "vivi-server-host-archive-unit-cfg-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: runs once, before any test reads the environment.
        unsafe { std::env::set_var("VIVI_CONFIG_DIR", &dir) };
    });
}

/// `label` reduced to a safe path-segment fragment.
fn safe_fragment(label: &str) -> String {
    label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

/// A unique safe project name: one path segment of lowercase letters,
/// digits and hyphens, unique across every fixture in the process.
fn unique_name(label: &str) -> String {
    let n = NAMES.fetch_add(1, Ordering::SeqCst);
    format!(
        "archive-unit-{}-{}-{n}",
        std::process::id(),
        safe_fragment(label)
    )
}

/// One fixture: a created project with its own scratch roots and clock.
pub(super) struct Fixture {
    /// The shared application state the handlers act on.
    pub(super) state: AppState,
    /// The store's clock in whole Unix seconds; store it forward to
    /// simulate elapsed time.
    pub(super) clock: Arc<AtomicI64>,
    /// The project's unique safe name.
    pub(super) name: String,
    /// The created project's id.
    pub(super) project_id: String,
    /// The project root (`<projects_root>/<name>`), whose `.vivi` is the
    /// mailspace.
    pub(super) root: PathBuf,
    /// The project's lanes directory (`<lanes_root>/<name>`).
    pub(super) lanes_dir: PathBuf,
    /// The server's own data directory (`DATA_DIR/projects/<id>/`).
    pub(super) data_dir: PathBuf,
    /// Held for the fixture's life: see [`SERIAL`].
    _serial: std::sync::MutexGuard<'static, ()>,
}

/// Create one project through the real parent `create_project` handler over
/// scratch roots and a fixed clock. Panics when creation fails: every test
/// needs the project.
pub(super) async fn fixture(label: &str, pump: bool) -> Fixture {
    let serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    isolate_registry();
    let name = unique_name(label);
    let base = std::env::temp_dir().join(format!(
        "vivi-server-host-archive-unit-{}-{}",
        std::process::id(),
        safe_fragment(label)
    ));
    let _ = std::fs::remove_dir_all(&base);
    let (projects, lanes) = (base.join("projects"), base.join("lanes"));
    let data_dir = base.join("data");
    std::fs::create_dir_all(&projects).unwrap();
    std::fs::create_dir_all(&lanes).unwrap();

    let clock = Arc::new(AtomicI64::new(BASE_SECONDS));
    let reading = clock.clone();
    let mut state = crate::server::app_state(data_dir.clone(), None, None).expect("app state");
    state.projects = ProjectStore::with_clock(&data_dir, move || {
        Utc.timestamp_opt(reading.load(Ordering::SeqCst), 0)
            .unwrap()
    });
    state.host = super::super::HostConfig {
        projects_root: Some(projects),
        lanes_root: Some(lanes),
        git_url_prefix: Some(GIT_URL_PREFIX.into()),
        default_pool: None,
    };

    let made = create_project(
        &state,
        &json!({
            "name": name,
            "initial_commit": true,
            "pump": pump,
            "serve": true,
        }),
    )
    .await
    .map_err(|e| e.message)
    .expect("the fixture project is created");
    Fixture {
        state,
        clock,
        project_id: made["project_id"].as_str().expect("project_id").to_owned(),
        root: PathBuf::from(made["project_root"].as_str().expect("project_root")),
        lanes_dir: PathBuf::from(made["lanes_dir"].as_str().expect("lanes_dir")),
        name,
        data_dir,
        _serial: serial,
    }
}

impl Fixture {
    /// The instant `seconds` Unix seconds after the epoch, as the
    /// whole-second RFC 3339 UTC string the server stamps write.
    pub(super) fn rfc3339(&self, seconds: i64) -> String {
        Utc.timestamp_opt(seconds, 0)
            .unwrap()
            .to_rfc3339_opts(SecondsFormat::Secs, true)
    }

    /// Stamp the archive record the archive handler would write — `at`
    /// read from the fixture clock, the mailspace root and lanes directory
    /// where they still lie — then unregister the project root and
    /// reconcile, so the project is retired and archived exactly like a
    /// real archive left it. Holds `CREATING` for its own mutation, as
    /// every host mutation does. Never calls the archive handler.
    pub(super) fn seed_archived(&self, pump: bool, serve: bool) {
        let _one = super::super::CREATING
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        self.state
            .projects
            .set_archived(
                &self.project_id,
                Some(Archived {
                    at: self.rfc3339(self.clock.load(Ordering::SeqCst)),
                    by: "mind".into(),
                    root: self.root.clone(),
                    lanes_dir: self.lanes_dir.clone(),
                    pump,
                    serve,
                }),
            )
            .expect("the archive record is stamped");
        crate::registry::update(|registry| Ok(registry.remove(&self.root)))
            .expect("the project root is unregistered");
        crate::server::hosted::reconcile_registry(&self.state.projects, &self.state.identities);
    }
}
