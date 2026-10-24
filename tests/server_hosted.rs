//! Hosted projects (S2): the server serves the registry's `serve = true`
//! roots straight from each project's own mailspace, with its per-project
//! data under `DATA_DIR/projects/<id>/` and no second mailspace.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::SigningKey;
use tower::util::ServiceExt;
use vivi::mailspace::Mailspace;
use vivi::registry::{self, Flags};
use vivi_server::hosted::{self, Report};
use vivi_server::identities::IdentityRegistry;
use vivi_server::ids::{mint_room_id, room_id_from_uuid};
use vivi_server::projects::{AppendItem, ProjectError, ProjectMeta, ProjectStore, Viewer};

fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("vivi-server-hosted-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Env {
    data: PathBuf,
    hosts: PathBuf,
    projects: ProjectStore,
    identities: IdentityRegistry,
}

fn env(name: &str) -> Env {
    let base = scratch(name);
    let data = base.join("data");
    let hosts = base.join("hosts");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&hosts).unwrap();
    Env {
        projects: ProjectStore::new(&data),
        identities: IdentityRegistry::new(&data),
        data,
        hosts,
    }
}

/// A host project root with an initialized mailspace; `sync` is appended to
/// `mailspace.toml` as the `[sync]` table body when given.
fn host_root(env: &Env, dir: &str, sync: Option<&str>) -> (PathBuf, Mailspace) {
    let root = env.hosts.join(dir);
    let mailspace = Mailspace::init(Some(&root)).unwrap();
    if let Some(body) = sync {
        let path = mailspace.config_path();
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str(&format!("\n[sync]\n{body}\n"));
        std::fs::write(&path, text).unwrap();
    }
    let mailspace = Mailspace::open_root(root.clone()).unwrap();
    (root, mailspace)
}

fn serve(env: &Env, roots: &[&Path]) -> Report {
    let set: BTreeSet<PathBuf> = roots.iter().map(|r| r.to_path_buf()).collect();
    hosted::reconcile(&env.projects, &env.identities, &set)
}

fn spki_pem(seed: u8) -> String {
    const PREFIX: &[u8] = &[
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    let mut der = PREFIX.to_vec();
    der.extend_from_slice(
        SigningKey::from_bytes(&[seed; 32])
            .verifying_key()
            .as_bytes(),
    );
    format!(
        "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----",
        STANDARD.encode(der)
    )
}

fn eml(id: &str) -> Vec<u8> {
    format!(
        "From: a@p.local\nTo: x@p.local\nMessage-ID: <{id}@p.local>\n\
         X-Vivi-Kind: mail\nX-Vivi-Format: 1\n\nbody"
    )
    .into_bytes()
}

fn project_dir(env: &Env, id: &str) -> PathBuf {
    env.data.join("projects").join(id)
}

#[test]
fn hosts_a_root_under_the_id_derived_from_its_mailspace() {
    let env = env("derived");
    let (root, mailspace) = host_root(&env, "app", None);
    let toml_before = std::fs::read(mailspace.config_path()).unwrap();

    let report = serve(&env, &[&root]);
    let id = room_id_from_uuid(mailspace.mailspace_id()).unwrap();
    assert!(report.refused.is_empty(), "{report:?}");
    assert_eq!(report.served.len(), 1);
    assert_eq!(report.served[0].project_id, id);

    // Opens as a hosted project whose mailspace is the host root's.
    let meta = env.projects.load_meta(&id).unwrap();
    assert!(meta.is_hosted());
    assert_eq!(meta.mailspace_root.as_deref(), Some(root.as_path()));
    assert!(meta.epoch.is_some());
    assert_eq!(env.projects.mailspace_dir(&id).unwrap(), root.join(".vivi"));

    // Server data lives under DATA_DIR; no mailspace was created there.
    let dir = project_dir(&env, &id);
    for file in [
        "project.json",
        "transport.sqlite",
        "identities.json",
        "admissions.json",
    ] {
        assert!(dir.join(file).is_file(), "{file}");
    }
    assert!(!dir.join(".vivi").exists(), "no .vivi under DATA_DIR");

    // The mailspace's own key is the creator member.
    let members = env.identities.list(&id);
    assert_eq!(members.len(), 1);
    assert_eq!(
        members[0].mailspace_id.as_deref(),
        Some(mailspace.mailspace_id())
    );
    assert_eq!(members[0].name, report.served[0].member);
    assert_eq!(members[0].name, mailspace.config.name);
    assert!(env.projects.admissions().is_admitted(&id, &members[0].name));

    // Hosting never rewrote the host's config.
    assert_eq!(std::fs::read(mailspace.config_path()).unwrap(), toml_before);

    // Reads work (an empty transport index: nothing in the mailspace travels yet).
    let viewer = Viewer {
        name: &members[0].name,
        prefix: None,
    };
    let page = env.projects.read_after_scoped(&id, 0, &viewer).unwrap();
    assert!(page.records.is_empty());
    assert_eq!(page.epoch, meta.epoch.unwrap());
}

#[test]
fn sync_project_and_name_win_and_a_repeat_pass_changes_nothing() {
    let env = env("configured");
    let id = mint_room_id().unwrap();
    let (root, mailspace) = host_root(
        &env,
        "app",
        Some(&format!("project = \"{id}\"\nname = \"hub-seat\"")),
    );

    let first = serve(&env, &[&root]);
    assert_eq!(first.served[0].project_id, id);
    assert_eq!(first.served[0].member, "hub-seat");
    let epoch = env.projects.epoch(&id).unwrap();
    let identities_before = std::fs::read(project_dir(&env, &id).join("identities.json")).unwrap();

    // A second pass, and a fresh store over the same data dir (a restart).
    serve(&env, &[&root]);
    let restarted = ProjectStore::new(&env.data);
    let again = hosted::reconcile(
        &restarted,
        &IdentityRegistry::new(&env.data),
        &BTreeSet::from([root.clone()]),
    );
    assert_eq!(again.served.len(), 1);
    assert_eq!(restarted.epoch(&id).unwrap(), epoch);
    assert_eq!(env.identities.list(&id).len(), 1);
    assert_eq!(
        std::fs::read(project_dir(&env, &id).join("identities.json")).unwrap(),
        identities_before,
        "member registration is idempotent"
    );
    assert_eq!(
        env.identities.list(&id)[0].mailspace_id.as_deref(),
        Some(mailspace.mailspace_id())
    );
}

#[test]
fn a_root_leaving_the_registry_stops_being_served_and_keeps_its_data() {
    let env = env("removed");
    let (root, _) = host_root(&env, "app", None);
    let id = serve(&env, &[&root]).served[0].project_id.clone();
    let epoch = env.projects.epoch(&id).unwrap();

    let report = serve(&env, &[]);
    assert_eq!(report.retired, vec![id.clone()]);
    assert_eq!(
        env.projects.existing_project_dir(&id),
        Err(ProjectError::ProjectNotFound)
    );
    assert!(project_dir(&env, &id).join("project.json").is_file());
    assert!(project_dir(&env, &id).join("identities.json").is_file());
    assert!(root.join(".vivi").join("mailspace.toml").is_file());

    // Served again: same project, same epoch, same single member.
    let back = serve(&env, &[&root]);
    assert!(back.retired.is_empty());
    assert_eq!(env.projects.epoch(&id).unwrap(), epoch);
    assert_eq!(env.identities.list(&id).len(), 1);
}

#[tokio::test]
async fn the_http_surface_answers_unknown_once_a_root_is_unserved() {
    let env = env("http");
    let (root, _) = host_root(&env, "app", None);
    let id = serve(&env, &[&root]).served[0].project_id.clone();
    let state = vivi_server::app_state(env.data.clone(), None, None);
    let app = vivi_server::router(state.clone());
    let get = |app: axum::Router| {
        let uri = format!("/v1/projects/{id}/directory");
        async move {
            app.oneshot(Request::get(uri).body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status()
        }
    };
    // Served: the project exists, the request just is not authenticated.
    hosted::reconcile(
        &state.projects,
        &state.identities,
        &BTreeSet::from([root.clone()]),
    );
    assert_eq!(get(app.clone()).await, StatusCode::UNAUTHORIZED);
    hosted::reconcile(&state.projects, &state.identities, &BTreeSet::new());
    assert_eq!(get(app).await, StatusCode::NOT_FOUND);
}

#[test]
fn a_server_owned_project_is_untouched_and_keeps_working() {
    let env = env("plain");
    let meta = env
        .projects
        .create_project_with_creator(&spki_pem(7), "alice")
        .unwrap();
    let id = meta.project_id.clone();
    let item = |n: u64| {
        let mid = format!("msg_{n:08x}{:016x}", 0);
        AppendItem::new(&mid, "alice", None, Some("need"), eml(&mid)).unwrap()
    };
    env.projects.append(&id, &item(1)).unwrap();

    // Hosting something else changes nothing for it, and an empty served
    // set never retires it.
    let (root, _) = host_root(&env, "app", None);
    let report = serve(&env, &[&root]);
    assert_eq!(report.served.len(), 1);
    assert!(report.retired.is_empty());
    serve(&env, &[]);

    let after = env.projects.load_meta(&id).unwrap();
    assert!(!after.is_hosted());
    assert_eq!(after.epoch, meta.epoch);
    assert!(project_dir(&env, &id).join(".vivi").is_dir());
    env.projects.append(&id, &item(2)).unwrap();
    let viewer = Viewer {
        name: "alice",
        prefix: None,
    };
    let page = env.projects.read_after_scoped(&id, 0, &viewer).unwrap();
    assert_eq!(page.records.len(), 2);
}

#[test]
fn an_id_held_by_a_server_owned_project_is_refused() {
    let env = env("collision");
    let owned = env
        .projects
        .create_project_with_creator(&spki_pem(7), "alice")
        .unwrap();
    let owned_bytes =
        std::fs::read(project_dir(&env, &owned.project_id).join("project.json")).unwrap();
    let (root, _) = host_root(
        &env,
        "app",
        Some(&format!(
            "project = \"{}\"\nname = \"seat\"",
            owned.project_id
        )),
    );

    let report = serve(&env, &[&root]);
    assert!(report.served.is_empty());
    assert_eq!(report.refused.len(), 1);
    assert!(
        report.refused[0].reason.contains("already names a project"),
        "{report:?}"
    );
    assert_eq!(
        std::fs::read(project_dir(&env, &owned.project_id).join("project.json")).unwrap(),
        owned_bytes
    );
    assert!(
        env.identities.list(&owned.project_id).is_empty(),
        "no member was added to the owned project"
    );
    let meta: ProjectMeta = env.projects.load_meta(&owned.project_id).unwrap();
    assert!(!meta.is_hosted());
}

#[test]
fn two_roots_with_one_id_serve_only_the_first() {
    let env = env("twins");
    let id = mint_room_id().unwrap();
    let sync = format!("project = \"{id}\"\nname = \"seat\"");
    let (a, _) = host_root(&env, "a", Some(&sync));
    let (b, _) = host_root(&env, "b", Some(&sync));
    let report = serve(&env, &[&a, &b]);
    assert_eq!(report.served.len(), 1);
    assert_eq!(report.served[0].root, a);
    assert_eq!(report.refused.len(), 1);
    assert_eq!(report.refused[0].root, b);
}

#[test]
fn a_bad_sync_project_is_refused() {
    let env = env("badid");
    let (root, _) = host_root(&env, "app", Some("project = \"nope\""));
    let report = serve(&env, &[&root]);
    assert!(report.served.is_empty());
    assert!(report.refused[0].reason.contains("canonical project id"));
    assert!(
        !env.data.join("projects").exists()
            || std::fs::read_dir(env.data.join("projects"))
                .unwrap()
                .next()
                .is_none()
    );
}

#[test]
fn a_hosted_project_accepts_an_append_and_files_it_in_the_host() {
    let env = env("append");
    let (root, _) = host_root(&env, "app", None);
    let id = serve(&env, &[&root]).served[0].project_id.clone();
    let mid = format!("msg_{:08x}{:016x}", 1, 0);
    let item = AppendItem::new(&mid, "x", None, Some("need"), eml(&mid)).unwrap();
    let outcome = env.projects.append(&id, &item).unwrap();
    assert!(!outcome.duplicate);
    assert!(env.projects.append(&id, &item).unwrap().duplicate);
    // The host mailspace holds the letter (an address of no local role).
    let storage = vivi::storage::Storage::open_mailspace(&root.join(".vivi")).unwrap();
    assert_eq!(storage.list_messages().unwrap().len(), 1);
}

/// A need letter with `front` as its front matter; `format_header` adds
/// `X-Vivi-Format: 1`, without it the letter is legacy.
fn need_eml(id: &str, format_header: bool, front: &str) -> Vec<u8> {
    let format = if format_header {
        "X-Vivi-Format: 1\n"
    } else {
        ""
    };
    format!(
        "From: a@p.local\nTo: x@p.local\nMessage-ID: <{id}@p.local>\n\
         X-Vivi-Kind: need\n{format}\n+++\n{front}\n+++\n\nbody"
    )
    .into_bytes()
}

/// Nothing held under the host mailspace's quarantine: no letter and no
/// receipt at the top of `.vivi/quarantine/` (the discard trail under
/// `dropped/` does not count).
fn assert_nothing_quarantined(vivi_dir: &Path) {
    assert!(vivi::sync::quarantine::list(vivi_dir).unwrap().is_empty());
    let held = std::fs::read_dir(vivi_dir.join("quarantine"))
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.path().is_file())
                .count()
        })
        .unwrap_or(0);
    assert_eq!(held, 0, "a held letter remains under .vivi/quarantine/");
}

#[test]
fn a_bad_format1_letter_is_refused_at_push_and_nothing_is_filed_or_held() {
    let env = env("quarantine-refused");
    let (root, _) = host_root(&env, "app", None);
    let id = serve(&env, &[&root]).served[0].project_id.clone();
    // An unknown front-matter key: format 1 is claimed, the parse refuses.
    let mid = format!("msg_{:08x}{:016x}", 2, 0);
    let item = AppendItem::new(
        &mid,
        "x",
        None,
        Some("need"),
        need_eml(&mid, true, "frob = \"nope\""),
    )
    .unwrap();

    let err = env.projects.append(&id, &item).unwrap_err();
    let ProjectError::BadRequest(message) = err else {
        panic!("a quarantined letter is refused as an append error, got {err:?}");
    };
    assert!(message.starts_with("letter refused: "), "{message}");
    assert!(message.contains("unknown field"), "{message}");
    assert!(message.contains("frob"), "{message}");

    // Nothing filed, and nothing held: the server discards its copy of a
    // letter it cannot read instead of quarantining it.
    let vivi_dir = root.join(".vivi");
    let storage = vivi::storage::Storage::open_mailspace(&vivi_dir).unwrap();
    assert_eq!(storage.list_messages().unwrap().len(), 0);
    assert_nothing_quarantined(&vivi_dir);
    let dropped = std::fs::read_dir(vivi_dir.join("quarantine").join("dropped"))
        .map(|entries| entries.flatten().count())
        .unwrap_or(0);
    assert_eq!(dropped, 2, "the held pair was discarded, not kept");
}

#[test]
fn a_good_format1_need_still_files_on_a_hosted_push() {
    let env = env("quarantine-good");
    let (root, _) = host_root(&env, "app", None);
    let id = serve(&env, &[&root]).served[0].project_id.clone();
    let mid = format!("msg_{:08x}{:016x}", 3, 0);
    let item = AppendItem::new(
        &mid,
        "x",
        None,
        Some("need"),
        need_eml(&mid, true, "done_when = [\"the smoke passes\"]"),
    )
    .unwrap();

    let outcome = env.projects.append(&id, &item).unwrap();
    assert!(!outcome.duplicate);
    let storage = vivi::storage::Storage::open_mailspace(&root.join(".vivi")).unwrap();
    assert_eq!(storage.list_messages().unwrap().len(), 1);
}

#[test]
fn a_legacy_letter_files_as_history_on_a_hosted_push() {
    // The hosted wire follows the filer's rule, not the durable push's:
    // a letter with no `X-Vivi-Format` header is history and is filed,
    // never refused.
    let env = env("quarantine-legacy");
    let (root, _) = host_root(&env, "app", None);
    let id = serve(&env, &[&root]).served[0].project_id.clone();
    let mid = format!("msg_{:08x}{:016x}", 4, 0);
    let item = AppendItem::new(
        &mid,
        "x",
        None,
        Some("need"),
        need_eml(&mid, false, "done_when = [\"the smoke passes\"]"),
    )
    .unwrap();

    let outcome = env.projects.append(&id, &item).unwrap();
    assert!(!outcome.duplicate);
    let storage = vivi::storage::Storage::open_mailspace(&root.join(".vivi")).unwrap();
    assert_eq!(storage.list_messages().unwrap().len(), 1);
}

#[tokio::test]
async fn the_watcher_follows_the_registry_file() {
    let env = env("watch");
    let (root, _) = host_root(&env, "app", None);
    let config = env.hosts.join("config");
    // This test alone sets the variable in this binary.
    unsafe { std::env::set_var("VIVI_CONFIG_DIR", &config) };
    let file = registry::path().unwrap();
    assert!(file.starts_with(&config));
    let save = |serve: bool| {
        let mut registry = registry::Registry::default();
        registry.set(root.clone(), Flags { pump: false, serve });
        registry::save_at(&file, &registry).unwrap();
    };
    let id = {
        let mailspace = Mailspace::open_root(root.clone()).unwrap();
        room_id_from_uuid(mailspace.mailspace_id()).unwrap()
    };

    // Registered but not served: nothing is hosted.
    save(false);
    let _watcher = hosted::start_with(
        env.projects.clone(),
        env.identities.clone(),
        Duration::from_millis(50),
    );
    assert_eq!(
        env.projects.existing_project_dir(&id),
        Err(ProjectError::ProjectNotFound)
    );

    save(true);
    wait_for(|| env.projects.existing_project_dir(&id).is_ok()).await;
    save(false);
    wait_for(|| env.projects.existing_project_dir(&id).is_err()).await;
    save(true);
    wait_for(|| env.projects.existing_project_dir(&id).is_ok()).await;
}

async fn wait_for(condition: impl Fn() -> bool) {
    for _ in 0..100 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the watcher did not converge within 5s");
}
