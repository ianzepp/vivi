//! Restore cases (`tests/host_archive.rs` child module): a restored
//! project is registered again under the flags saved at archive time, its
//! record is cleared (which restarts the 24-hour purge clock), and the
//! minted operator key reaches the project's tools again — all without any
//! `HOST_*` configuration. Refusal modes hold for a project that is not
//! archived and for an archived project whose root is gone (the record is
//! retained so the purge can still run from it).
//!
//! Clock discipline: the only time that moves is `Fx::clock`
//! (`Arc<AtomicI64>`); no archive stamp in metadata is ever rewritten and
//! the production delay is never touched.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::Ordering;

use serde_json::{Value, json};
use vivi::registry::{self, Flags};
use vivi::server::mcp::host::HostConfig;

use super::{BASE_SECONDS, Fx, PREFIX, fixture, rfc3339, router};

/// Twelve hours, the midpoint the restore case advances the clock by.
const TWELVE_HOURS: i64 = 12 * 60 * 60;

/// The hour count of the fixed purge delay, for clock arithmetic only.
const TWENTY_FOUR_HOURS: i64 = 24 * 60 * 60;

/// The `HostConfig` the fixture boots with, rebuilt by hand for the legs
/// that need `HOST_*` values again after a blanking.
fn host_config(fx: &Fx) -> HostConfig {
    HostConfig {
        projects_root: Some(fx.projects.clone()),
        lanes_root: Some(fx.lanes.clone()),
        git_url_prefix: Some(PREFIX.into()),
        default_pool: None,
    }
}

/// Create one project through the tool (`initial_commit`, `pump`, `serve`
/// as given) and return (project id, canonical root, minted operator key).
async fn create(fx: &Fx, name: &str, pump: bool, serve: bool) -> (String, PathBuf, String) {
    let (errored, made) = fx
        .create(json!({
            "name": name,
            "initial_commit": true,
            "pump": pump,
            "serve": serve,
        }))
        .await;
    assert!(!errored, "{made}");
    assert_eq!(made["served"], true, "{made}");
    let id = made["project_id"].as_str().unwrap().to_owned();
    let root: PathBuf = made["project_root"].as_str().unwrap().to_owned().into();
    let key = made["operator_key"]["key"].as_str().unwrap().to_owned();
    (id, root, key)
}

async fn archive(fx: &Fx, name: &str) -> Value {
    let (errored, archived) = fx
        .call_tool("archive_project", json!({"name": name, "confirm": name}))
        .await;
    assert!(!errored, "{archived}");
    archived
}

/// The `project_list` entry for `id`, required to exist.
async fn listed_entry(fx: &Fx, id: &str) -> Value {
    let (errored, listed) = fx.call_tool("project_list", json!({})).await;
    assert!(!errored, "{listed}");
    listed
        .as_array()
        .expect("project_list answers a list")
        .iter()
        .find(|item| item["project_id"] == json!(id))
        .unwrap_or_else(|| panic!("project {id} is listed: {listed:?}"))
        .clone()
}

/// A pump:false, serve:true project is archived at T, the clock advances
/// twelve hours, and a restore naming only the project brings it back: the
/// recorded root is registered with the saved flags (never
/// `Flags::default`), project.json no longer carries an archive record,
/// the same project id and epoch are retained, project_list reports hosted
/// again, and the original operator key reaches a project tool again.
///
/// Restoring runs against a blank `HostConfig` and a rebuilt router: the
/// archive record carries everything. The project is archived again at the
/// advanced clock; at the first archive's T + 24 hours the purge is still
/// refused and names the second archive's allowed instant, and past the
/// second archive's deadline the purge succeeds.
#[tokio::test]
async fn restore_registers_the_project_again_and_resets_the_purge_clock() {
    let mut fx = fixture("restore-clock");
    let name = "restore-clock";
    let (id, root, key) = create(&fx, name, false, true).await;

    // The flags creation registered: restore must put these back.
    let saved = registry::load()
        .unwrap()
        .get(&root)
        .expect("creation registered the root");
    assert_eq!(
        saved,
        Flags {
            pump: false,
            serve: true
        }
    );

    let meta = fx.project_json(&id);
    let epoch = meta["epoch"].clone();
    assert!(epoch.is_u64(), "{meta}");
    assert!(meta["archived"].is_null(), "{meta}");
    let lanes_dir = fx.lanes.canonicalize().unwrap().join(name);

    // Archived at the fixture's T.
    let archived = archive(&fx, name).await;
    assert_eq!(
        archived["archived"]["at"],
        json!(rfc3339(BASE_SECONDS)),
        "{archived}"
    );
    assert_eq!(archived["archived"]["pump"], false, "{archived}");
    assert_eq!(archived["archived"]["serve"], true, "{archived}");

    // While archived the minted key is refused on the project.
    let (errored, refused) = fx.call_as(&key, "pump_list", json!({"project": id})).await;
    assert!(errored, "{refused}");
    assert!(
        refused["error"].as_str().unwrap().contains("archived"),
        "{refused}"
    );

    // Restore needs no HOST_* value of any kind: blank the configuration
    // and rebuild the router before the clock moves.
    fx.state.host = HostConfig::default();
    fx.app = router(fx.state.clone());

    // Twelve hours after the archive, restore with only the name.
    fx.clock
        .store(BASE_SECONDS + TWELVE_HOURS, Ordering::SeqCst);
    let (errored, restored) = fx.call_tool("restore_project", json!({"name": name})).await;
    assert!(!errored, "{restored}");
    assert_eq!(restored["name"], json!(name), "{restored}");
    assert_eq!(restored["project_id"], json!(id), "{restored}");
    assert_eq!(
        restored["root"],
        json!(root.display().to_string()),
        "{restored}"
    );
    assert_eq!(
        restored["lanes_dir"],
        json!(lanes_dir.display().to_string()),
        "{restored}"
    );
    assert_eq!(restored["pump"], false, "{restored}");
    assert_eq!(restored["serve"], true, "{restored}");
    assert_eq!(restored["restored"], true, "{restored}");
    assert_eq!(restored["served"], true, "{restored}");
    assert!(restored.get("serve_error").is_none(), "{restored}");

    // The recorded root is registered with the original flags again.
    assert_eq!(registry::load().unwrap().get(&root), Some(saved));

    // project.json omits the archive record; the project id and its epoch
    // are exactly what creation made.
    let meta = fx.project_json(&id);
    assert!(meta["archived"].is_null(), "{meta}");
    assert_eq!(meta["project_id"], json!(id), "{meta}");
    assert_eq!(meta["epoch"], epoch, "{meta}");

    // project_list reports the project hosted again.
    let entry = listed_entry(&fx, &id).await;
    assert_eq!(entry["mode"], "hosted", "{entry}");
    assert_eq!(entry["name"], json!(name), "{entry}");

    // The original operator key reaches a project tool again.
    let (errored, listed) = fx.call_as(&key, "pump_list", json!({"project": id})).await;
    assert!(!errored, "{listed}");
    assert_eq!(listed["total"], 0, "{listed}");

    // Archive again at the advanced clock: a fresh record stamped T + 12h.
    // This leg needs HOST_LANES_ROOT, so the fixture configuration returns
    // for it; the restore above already ran without any of it.
    fx.state.host = host_config(&fx);
    fx.app = router(fx.state.clone());
    let second_at = BASE_SECONDS + TWELVE_HOURS;
    let archived = archive(&fx, name).await;
    assert_eq!(
        archived["archived"]["at"],
        json!(rfc3339(second_at)),
        "{archived}"
    );

    // The purge clock restarted with the second archive: at the FIRST
    // archive's T + 24 hours the purge is still refused, naming the second
    // archive's allowed instant. No timestamp was rewritten by hand.
    fx.state.host = HostConfig::default();
    fx.app = router(fx.state.clone());
    fx.clock
        .store(BASE_SECONDS + TWENTY_FOUR_HOURS, Ordering::SeqCst);
    let (errored, refused) = fx
        .call_tool("purge_project", json!({"name": name, "confirm": name}))
        .await;
    assert!(errored, "{refused}");
    let allowed = rfc3339(second_at + TWENTY_FOUR_HOURS);
    assert!(
        refused["error"].as_str().unwrap().contains(&allowed),
        "the refusal names the second archive's allowed instant: {refused}"
    );
    assert!(root.is_dir(), "a refused purge removes nothing");

    // Past the second archive's deadline the purge succeeds and the
    // project is gone for good.
    fx.clock
        .store(second_at + TWENTY_FOUR_HOURS + 1, Ordering::SeqCst);
    let (errored, purged) = fx
        .call_tool("purge_project", json!({"name": name, "confirm": name}))
        .await;
    assert!(!errored, "{purged}");
    assert_eq!(purged["project_id"], json!(id), "{purged}");
    assert!(!root.exists(), "the purge removed the project root");
    assert!(
        !fx.lanes.join(name).exists(),
        "the purge removed the lanes directory"
    );
    assert!(
        !fx.data.join("projects").join(&id).exists(),
        "the purge removed the server's project directory"
    );
}

/// A served, never-archived project refuses the restore and is untouched:
/// the registry keeps its flags and no archive record appears.
#[tokio::test]
async fn restore_refuses_a_project_that_is_not_archived() {
    let fx = fixture("restore-live");
    let name = "restore-live";
    let (id, root, _key) = create(&fx, name, true, true).await;
    let before = registry::load().unwrap().get(&root);

    let (errored, value) = fx.call_tool("restore_project", json!({"name": name})).await;
    assert!(errored, "{value}");
    assert!(
        value["error"].as_str().unwrap().contains("not archived"),
        "{value}"
    );

    assert_eq!(registry::load().unwrap().get(&root), before);
    assert_eq!(
        registry::load().unwrap().get(&root),
        Some(Flags {
            pump: true,
            serve: true
        })
    );
    let meta = fx.project_json(&id);
    assert!(meta["archived"].is_null(), "{meta}");
    assert_eq!(listed_entry(&fx, &id).await["mode"], "hosted");
}

/// An archived project whose mailspace root disappeared refuses the
/// restore with the recorded root named, and the archive record is
/// retained exactly as stamped — still unregistered, still listed
/// archived — so the purge can still run from it at its deadline.
#[tokio::test]
async fn restore_refuses_when_the_recorded_root_is_gone_and_keeps_the_record() {
    let fx = fixture("restore-gone-root");
    let name = "restore-gone-root";
    let (id, root, _key) = create(&fx, name, true, true).await;
    let lanes_dir = fx.lanes.canonicalize().unwrap().join(name);

    let archived = archive(&fx, name).await;
    assert_eq!(
        archived["archived"]["at"],
        json!(rfc3339(BASE_SECONDS)),
        "{archived}"
    );

    // The mailspace root disappears under the archive, out of band.
    fs::remove_dir_all(&root).unwrap();
    assert!(!root.exists());

    let (errored, value) = fx.call_tool("restore_project", json!({"name": name})).await;
    assert!(errored, "{value}");
    let message = value["error"].as_str().unwrap();
    assert!(message.contains("no longer exists"), "{value}");
    assert!(message.contains(&root.display().to_string()), "{value}");

    // The record stands exactly as archive stamped it, for purge.
    let meta = fx.project_json(&id);
    let record = &meta["archived"];
    assert_eq!(record["at"], json!(rfc3339(BASE_SECONDS)), "{meta}");
    assert_eq!(record["root"], json!(root.display().to_string()), "{meta}");
    assert_eq!(
        record["lanes_dir"],
        json!(lanes_dir.display().to_string()),
        "{meta}"
    );
    assert!(
        registry::load().unwrap().get(&root).is_none(),
        "the refused restore does not register the missing root"
    );
    let entry = listed_entry(&fx, &id).await;
    assert_eq!(entry["mode"], "archived", "{entry}");

    // The retained record is enough for the purge: at the deadline it
    // removes what is left of the project.
    fx.clock
        .store(BASE_SECONDS + TWENTY_FOUR_HOURS, Ordering::SeqCst);
    let (errored, purged) = fx
        .call_tool("purge_project", json!({"name": name, "confirm": name}))
        .await;
    assert!(!errored, "{purged}");
    assert!(!lanes_dir.exists(), "the purge removed the lanes directory");
    assert!(
        !fx.data.join("projects").join(&id).exists(),
        "the purge removed the server's project directory"
    );
}
