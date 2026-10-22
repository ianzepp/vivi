//! Hosted projects: the server serves the projects its host registry lists
//! (`serve = true`, the registry the pump supervisor also reads) directly
//! from each project's own mailspace. There is no second mailspace and no
//! seeding.
//!
//! A hosted project keeps the server's per-project data where every project
//! keeps it, `DATA_DIR/projects/<id>/` (`project.json`, `transport.sqlite`,
//! `identities.json`, `admissions.json`, `keys.json`), but its mailspace is
//! the host root's own `.vivi/`, named by `mailspace_root` in `project.json`.
//! The server never initializes, hardens, or otherwise lays out a host
//! root.
//!
//! The project id is `[sync] project` from the mailspace's `mailspace.toml`
//! when it is set, otherwise the id derived from the mailspace's own UUID
//! ([`room_id_from_uuid`]), which is the same 25 Crockford digits every
//! project id has, so the HTTP routes accept it. The mailspace's own sync
//! key (the one `vivi sync key` prints) is admitted and registered as the
//! project's creator member, named by `[sync] name` or else the mailspace
//! name.
//!
//! [`reconcile`] makes the served set match the registry; [`start`] runs it
//! once and then again whenever the registry file changes. A root that
//! leaves the registry stops being served and loses nothing on disk.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use vivi::mailspace::Mailspace;
use vivi::registry;
use vivi::sync::identity::SyncIdentity;

use crate::auth::{canonical_spki_hex, key_fingerprint, parse_verifying_key};
use crate::crypto::harden_tree;
use crate::identities::IdentityRegistry;
use crate::ids::{normalize_room_id, room_id_from_uuid};
use crate::projects::{ProjectMeta, ProjectStore};

/// How often the watcher looks at the registry file.
pub const POLL_INTERVAL: Duration = Duration::from_secs(3);

/// A registry root the server now serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Served {
    pub project_id: String,
    pub root: PathBuf,
    /// The creator member registered for the mailspace's own key.
    pub member: String,
}

/// A registry root the server declined to serve, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub root: PathBuf,
    pub reason: String,
}

/// What one [`reconcile`] pass did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    pub served: Vec<Served>,
    pub refused: Vec<Refused>,
    /// Hosted projects on disk whose root is no longer served.
    pub retired: Vec<String>,
}

/// Make the hosted projects match `roots` (the registry's `served()` set):
/// serve each root that resolves, refuse the ones that do not (each with a
/// log line), and stop serving every hosted project whose root is not
/// served. Idempotent.
pub fn reconcile(
    projects: &ProjectStore,
    identities: &IdentityRegistry,
    roots: &BTreeSet<PathBuf>,
) -> Report {
    let mut report = Report::default();
    let mut claimed: HashMap<String, PathBuf> = HashMap::new();
    for root in roots {
        match serve_root(projects, identities, root, &claimed) {
            Ok(served) => {
                tracing::info!(
                    "hosting project {} from {} (member {})",
                    served.project_id,
                    root.display(),
                    served.member
                );
                claimed.insert(served.project_id.clone(), root.clone());
                report.served.push(served);
            }
            Err(reason) => {
                tracing::error!("not hosting {}: {reason}", root.display());
                report.refused.push(Refused {
                    root: root.clone(),
                    reason,
                });
            }
        }
    }
    let retired = hosted_ids_on_disk(projects)
        .into_iter()
        .filter(|id| !claimed.contains_key(id))
        .collect::<HashSet<_>>();
    for id in &retired {
        tracing::info!("not serving hosted project {id}: its root is no longer served");
    }
    report.retired = sorted(&retired);
    projects.set_retired(retired);
    report
}

fn sorted(ids: &HashSet<String>) -> Vec<String> {
    let mut ids: Vec<String> = ids.iter().cloned().collect();
    ids.sort();
    ids
}

/// Resolve and host one root.
fn serve_root(
    projects: &ProjectStore,
    identities: &IdentityRegistry,
    root: &Path,
    claimed: &HashMap<String, PathBuf>,
) -> Result<Served, String> {
    let mailspace = Mailspace::open_root(root.to_path_buf())
        .map_err(|error| format!("cannot open its mailspace: {error}"))?;
    let project_id = resolve_project_id(&mailspace)?;
    if let Some(other) = claimed.get(&project_id) {
        return Err(format!(
            "project id {project_id} is already hosted from {}",
            other.display()
        ));
    }
    let dir = projects
        .project_dir(&project_id)
        .map_err(|error| format!("project id {project_id} is unusable: {error:?}"))?;
    let existing = read_meta(&dir)?;
    if existing.as_ref().is_some_and(|meta| !meta.is_hosted()) {
        return Err(format!(
            "project id {project_id} already names a project the server owns; \
             that project is untouched"
        ));
    }
    let member = admit_creator(projects, identities, &mailspace, &project_id)?;
    let current = existing
        .as_ref()
        .and_then(|meta| meta.mailspace_root.as_deref());
    if current != Some(root) {
        if let Some(old) = current {
            tracing::warn!(
                "hosted project {project_id} moves from {} to {}",
                old.display(),
                root.display()
            );
        }
        write_meta(&dir, existing, &project_id, root)?;
    }
    projects
        .prepare_hosted(&dir, root)
        .map_err(|error| format!("cannot open the project: {error:?}"))?;
    Ok(Served {
        project_id,
        root: root.to_path_buf(),
        member,
    })
}

/// `[sync] project` when set, else the id derived from the mailspace id.
fn resolve_project_id(mailspace: &Mailspace) -> Result<String, String> {
    let configured = mailspace
        .config
        .sync
        .as_ref()
        .and_then(|sync| sync.project.as_deref())
        .map(str::trim)
        .filter(|project| !project.is_empty());
    match configured {
        Some(project) => normalize_room_id(project).map_err(|_| {
            format!(
                "[sync] project '{project}' is not a canonical project id (25 Crockford digits)"
            )
        }),
        None => room_id_from_uuid(mailspace.mailspace_id()).ok_or_else(|| {
            format!(
                "mailspace id '{}' is not a canonical UUID",
                mailspace.mailspace_id()
            )
        }),
    }
}

/// The creator member's name: `[sync] name`, else the mailspace name, as an
/// identity name (letters, digits, dot, dash, underscore; at most 64).
fn member_name(mailspace: &Mailspace) -> String {
    let raw = mailspace
        .config
        .sync
        .as_ref()
        .and_then(|sync| sync.name.as_deref())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(mailspace.config.name.trim());
    let name: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .take(64)
        .collect();
    if name.is_empty() || name.eq_ignore_ascii_case("postmaster") {
        "hub".to_owned()
    } else {
        name
    }
}

/// Admit the mailspace's own sync key as the creator and register it with
/// the mailspace's id, as a client's `vivi sync create` then `join` would.
/// A key already registered under this mailspace id is the same member and
/// changes nothing. Returns the member's name.
fn admit_creator(
    projects: &ProjectStore,
    identities: &IdentityRegistry,
    mailspace: &Mailspace,
    project_id: &str,
) -> Result<String, String> {
    let identity = SyncIdentity::ensure(&mailspace.dir)
        .map_err(|error| format!("cannot load the mailspace sync key: {error}"))?;
    let pem = identity.public_key_pem();
    let key = parse_verifying_key(&pem).map_err(|_| "the mailspace sync key is invalid")?;
    let fingerprint = key_fingerprint(&key).map_err(|_| "the mailspace sync key is invalid")?;
    let spki = canonical_spki_hex(&key).map_err(|_| "the mailspace sync key is invalid")?;
    let mailspace_id = mailspace.mailspace_id();
    if let Some(member) = identities
        .list(project_id)
        .into_iter()
        .find(|member| member.mailspace_id.as_deref() == Some(mailspace_id))
    {
        return if member.fingerprint == fingerprint {
            Ok(member.name)
        } else {
            Err(format!(
                "member {} already holds this mailspace id under another key",
                member.name
            ))
        };
    }
    let name = member_name(mailspace);
    if let Some(taken) = identities.by_name(project_id, &name) {
        let hint = if taken.spki_der == spki {
            "with this key but another mailspace id"
        } else {
            "for another key"
        };
        return Err(format!("member name {name} is already registered {hint}"));
    }
    projects
        .admissions()
        .admit(project_id, &name, &pem, "creator")
        .map_err(|error| format!("cannot admit the creator {name}: {error:?}"))?;
    identities
        .register(project_id, &name, &pem, mailspace_id)
        .map_err(|error| format!("cannot register the creator {name}: {error:?}"))?;
    Ok(name)
}

/// `project.json` in `dir`, if the project exists.
fn read_meta(dir: &Path) -> Result<Option<ProjectMeta>, String> {
    match std::fs::read(dir.join("project.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| format!("unreadable project.json in {}: {error}", dir.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "cannot read project.json in {}: {error}",
            dir.display()
        )),
    }
}

/// Create or re-point the hosted project's `project.json` (and prepare its
/// data directory). The epoch of an existing project is kept.
fn write_meta(
    dir: &Path,
    existing: Option<ProjectMeta>,
    project_id: &str,
    root: &Path,
) -> Result<(), String> {
    let meta = match existing {
        Some(mut meta) => {
            meta.mailspace_root = Some(root.to_path_buf());
            meta
        }
        None => ProjectMeta::hosted(project_id.to_owned(), root.to_path_buf())
            .map_err(|error| format!("cannot mint the project: {error:?}"))?,
    };
    let bytes = serde_json::to_vec(&meta).map_err(|error| format!("cannot encode: {error}"))?;
    std::fs::create_dir_all(dir)
        .and_then(|()| std::fs::write(dir.join("project.json"), bytes))
        .map_err(|error| format!("cannot write project.json in {}: {error}", dir.display()))?;
    // Only the server's own data directory; the host root is never touched.
    harden_tree(dir);
    Ok(())
}

/// Ids of every hosted project with a `project.json` under the data root.
fn hosted_ids_on_disk(projects: &ProjectStore) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(projects.projects_root()) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let meta = read_meta(&entry.path()).ok().flatten()?;
            meta.is_hosted().then_some(meta.project_id)
        })
        .collect()
}

/// Load the host registry and reconcile against its served roots. A registry
/// that cannot be read changes nothing.
pub fn reconcile_registry(
    projects: &ProjectStore,
    identities: &IdentityRegistry,
) -> Option<Report> {
    match registry::load() {
        Ok(registry) => Some(reconcile(projects, identities, &registry.served())),
        Err(error) => {
            tracing::warn!("host registry unreadable; hosted projects unchanged: {error}");
            None
        }
    }
}

/// Reconcile once now, then reconcile again whenever the registry file
/// changes, checking every [`POLL_INTERVAL`]. Must run inside a tokio
/// runtime; the first pass finishes before this returns.
pub fn start(projects: ProjectStore, identities: IdentityRegistry) -> tokio::task::JoinHandle<()> {
    start_with(projects, identities, POLL_INTERVAL)
}

/// [`start`] with an explicit poll interval.
pub fn start_with(
    projects: ProjectStore,
    identities: IdentityRegistry,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    // Read the marker before the first pass so a change during it is seen.
    let mut last = registry::fingerprint().ok();
    reconcile_registry(&projects, &identities);
    tokio::spawn(async move {
        let mut unreadable = false;
        loop {
            tokio::time::sleep(interval).await;
            match registry::fingerprint() {
                Ok(now) => {
                    unreadable = false;
                    if last != Some(now) {
                        last = Some(now);
                        let (projects, identities) = (projects.clone(), identities.clone());
                        let _ = tokio::task::spawn_blocking(move || {
                            reconcile_registry(&projects, &identities)
                        })
                        .await;
                    }
                }
                Err(error) => {
                    if !unreadable {
                        tracing::warn!("host registry unreadable: {error}");
                    }
                    unreadable = true;
                }
            }
        }
    })
}
