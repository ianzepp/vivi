//! Purge cleanup cases (`tests/host_archive.rs` child module): once the
//! 24-hour delay has passed, a purge destroys an archived project for
//! good — the recorded mailspace root, the recorded lanes directory and
//! the server's own `DATA_DIR/projects/<id>` all go away, the registry
//! keeps no entry, the id leaves `project_list`, and every grant on the
//! project is dropped (a principal left without any grant has its keys
//! revoked). A purge runs from the archive record alone: the case blanks
//! the fixture `HostConfig` and rebuilds the router before purging, so no
//! `HOST_*` setting is consulted. A live pump `.vivi/pump/lock` refuses
//! the purge while keeping every tree and grant, and dropping the lock
//! lets the same purge through; trees a partial earlier attempt already
//! removed do not stop the data and grant cleanup.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::Ordering;

use serde_json::{Value, json};
use vivi::registry;
use vivi_server::mcp::host::HostConfig;

use super::{BASE_SECONDS, fixture, router};

/// The purge delay under test, in seconds: one day.
const DAY_SECONDS: i64 = 24 * 60 * 60;

/// The project tree triple a refused purge must leave untouched: the
/// host projects root (the mailspace), the lanes root, and the server's
/// own `DATA_DIR/projects` (the archive record lives there). The audit
/// log beside `projects/` is excluded: a refusal itself writes one line.
fn trees(fx: &super::Fx) -> (Vec<(String, u64)>, Vec<(String, u64)>, Vec<(String, u64)>) {
    (
        super::snapshot(&fx.projects),
        super::snapshot(&fx.lanes),
        super::snapshot(&fx.data.join("projects")),
    )
}

/// Grants and usable keys across the whole store, as comparable JSON
/// (digests only; `revoked_at` included, so a revocation shows).
fn keys(fx: &super::Fx) -> Value {
    serde_json::to_value(fx.state.server_keys.list()).unwrap()
}

/// Principals holding a grant on `id`, in store order.
fn holders(fx: &super::Fx, id: &str) -> Vec<String> {
    fx.state
        .server_keys
        .list()
        .into_iter()
        .filter(|p| p.grants.iter().any(|g| g.project_id == id))
        .map(|p| p.principal)
        .collect()
}

/// The `project_list` entry for `id`, when it is still listed.
async fn listed_entry(fx: &super::Fx, id: &str) -> Option<Value> {
    let (_, listed) = fx.call_tool("project_list", json!({})).await;
    listed
        .as_array()
        .expect("project_list answers an array")
        .iter()
        .find(|item| item["project_id"] == json!(id))
        .cloned()
}

/// Create a served project through the host operator token: the answer
/// carries the project id, the creator member and the minted operator
/// key, whose principal `<member>@<project_id>` is the project's only
/// grant holder.
async fn create_with_operator(fx: &super::Fx, name: &str) -> (String, String, String) {
    let (errored, made) = fx
        .create(json!({ "name": name, "initial_commit": true }))
        .await;
    assert!(!errored, "{made}");
    assert_eq!(made["served"], true, "{made}");
    let id = made["project_id"].as_str().unwrap().to_owned();
    let member = made["member"].as_str().unwrap().to_owned();
    let key = made["operator_key"]["key"].as_str().unwrap().to_owned();
    let principal = format!("{member}@{id}");
    (id, principal, key)
}

/// Archive through the tool and return the answer (`root` and
/// `lanes_dir` are the recorded paths).
async fn archive(fx: &super::Fx, name: &str) -> Value {
    let (errored, archived) = fx
        .call_tool("archive_project", json!({ "name": name, "confirm": name }))
        .await;
    assert!(!errored, "{archived}");
    archived
}

/// The purge call with the name repeated as confirm.
async fn purge(fx: &super::Fx, name: &str) -> (bool, Value) {
    fx.call_tool("purge_project", json!({ "name": name, "confirm": name }))
        .await
}

/// A held pump lock refuses the purge however old the archive is: the
/// refusal identifies the lock, the three trees and every grant are
/// exactly as they were, and dropping the lock lets the same purge
/// succeed and remove the project for good.
#[tokio::test]
async fn purge_is_refused_while_a_pump_holds_the_lock() {
    let fx = fixture("purge-cleanup-lock");
    let name = "purge-cleanup-lock";
    let (id, _operator, _key) = create_with_operator(&fx, name).await;

    let archived = archive(&fx, name).await;
    let root = PathBuf::from(archived["root"].as_str().unwrap());
    let lock_path = root.join(".vivi").join("pump").join("lock");

    // Well past the deadline: only the lock can refuse now.
    fx.clock
        .store(BASE_SECONDS + DAY_SECONDS + 1, Ordering::SeqCst);

    // A pump mid-tick holds the mailspace's real `.vivi/pump/lock`.
    let lock = fx.hold_pump_lock(name);
    let before = (trees(&fx), keys(&fx));

    let (errored, answer) = purge(&fx, name).await;
    assert!(errored, "{answer}");
    let error = answer["error"].as_str().unwrap();
    assert!(
        error.contains(&lock_path.display().to_string()),
        "the refusal identifies the lock: {error}"
    );
    assert!(
        error.contains("pump"),
        "the refusal names the pump: {error}"
    );

    let (trees_after, keys_after) = (trees(&fx), keys(&fx));
    assert_eq!(trees_after, before.0, "the three trees stay");
    assert_eq!(keys_after, before.1, "grants and usable keys stay");
    assert!(root.exists(), "the mailspace root stays");
    assert!(fx.lanes.join(name).exists(), "the lanes directory stays");
    assert!(
        fx.data.join("projects").join(&id).exists(),
        "the server's project directory stays"
    );

    // The same purge, the lock dropped: it goes through.
    drop(lock);
    let (errored, purged) = purge(&fx, name).await;
    assert!(!errored, "{purged}");
    assert_eq!(purged["project_id"], json!(id), "{purged}");
    assert!(!root.exists(), "the purge removed the project root");
    assert!(!fx.lanes.join(name).exists(), "the purge removed the lanes");
    assert!(
        !fx.data.join("projects").join(&id).exists(),
        "the purge removed the server's project directory"
    );
}

/// Past the deadline a purge removes the recorded root, the recorded
/// lanes directory and `DATA_DIR/projects/<id>`, leaves no registry
/// entry, takes the id out of `project_list`, and drops the project's
/// grants: the operator principal that held the project as its final
/// grant is ungranted with its key revoked, while a principal granted
/// only on another project keeps both its grant and a usable key. The
/// purge runs against a blank `HostConfig` and a rebuilt router —
/// everything comes from the archive record, no `HOST_*` setting.
#[tokio::test]
async fn purge_after_the_delay_removes_every_tree_and_the_grants() {
    let mut fx = fixture("purge-cleanup-full");
    let name = "purge-cleanup-full";
    let (id, operator_principal, operator_key) = create_with_operator(&fx, name).await;

    // A lane on the project: its bare repository lands in the directory
    // the archive records as `lanes_dir`.
    let (errored, lane) = fx
        .call_tool(
            "lane_create",
            json!({ "name": name, "target": "sandbox", "seed": true }),
        )
        .await;
    assert!(!errored, "{lane}");
    assert!(fx.lanes.join(name).join("sandbox.git").is_dir(), "{lane}");

    // A principal whose only grant is on another project: the purge must
    // not touch it.
    let (errored, minted) = fx
        .call_tool(
            "key_mint",
            json!({
                "principal": "keeper@other-project",
                "label": "keeper",
                "grants": [{ "project_id": "other-project", "member": "keeper" }],
            }),
        )
        .await;
    assert!(!errored, "{minted}");
    let keeper_key = minted["key"].as_str().unwrap().to_owned();

    let archived = archive(&fx, name).await;
    let root = PathBuf::from(archived["root"].as_str().unwrap());
    let lanes_dir = PathBuf::from(archived["lanes_dir"].as_str().unwrap());
    assert!(
        registry::load().unwrap().get(&root).is_none(),
        "the archive unregistered the root"
    );

    // The cleanup case: purge needs no `HOST_*` value of any kind.
    fx.state.host = HostConfig::default();
    fx.app = router(fx.state.clone());

    // Only the injected clock moves past the deadline.
    fx.clock
        .store(BASE_SECONDS + DAY_SECONDS + 1, Ordering::SeqCst);

    // The operator principal is the project's only grant holder.
    assert_eq!(
        holders(&fx, &id),
        vec![operator_principal.clone()],
        "grants before the purge"
    );
    let entry = listed_entry(&fx, &id).await.expect("still listed");
    assert_eq!(entry["mode"], "archived", "{entry}");

    let (errored, purged) = purge(&fx, name).await;
    assert!(!errored, "{purged}");
    assert_eq!(purged["name"], json!(name), "{purged}");
    assert_eq!(purged["project_id"], json!(id), "{purged}");
    assert_eq!(
        purged["project_root"],
        json!(root.display().to_string()),
        "{purged}"
    );
    assert_eq!(
        purged["lanes_dir"],
        json!(lanes_dir.display().to_string()),
        "{purged}"
    );
    assert_eq!(
        purged["data_dir"],
        json!(fx.data.join("projects").join(&id)),
        "{purged}"
    );
    assert_eq!(
        purged["ungranted"],
        json!([operator_principal]),
        "the operator principal was ungranted: {purged}"
    );
    assert!(
        purged["keys_revoked"].as_u64().unwrap_or(0) >= 1,
        "at least one key was revoked: {purged}"
    );

    // Every tree is gone: the recorded root, the recorded lanes
    // directory, and the server's own project directory.
    assert!(!root.exists(), "the recorded root is gone");
    assert!(!lanes_dir.exists(), "the recorded lanes directory is gone");
    assert!(
        !fx.data.join("projects").join(&id).exists(),
        "the server's project directory is gone"
    );

    // No registry entry, and the id left project_list.
    assert!(registry::load().unwrap().get(&root).is_none());
    assert!(
        listed_entry(&fx, &id).await.is_none(),
        "the purged id is no longer listed"
    );

    // No principal holds a grant on the purged id any more.
    assert!(holders(&fx, &id).is_empty(), "every grant went");

    // The keeper keeps exactly its unrelated grant, on a usable key.
    let verified = fx
        .state
        .server_keys
        .verify(&keeper_key)
        .expect("the keeper key still verifies");
    assert_eq!(
        verified
            .grants
            .iter()
            .map(|g| g.project_id.as_str())
            .collect::<Vec<_>>(),
        vec!["other-project"],
        "the keeper's unrelated grant survives: {:?}",
        verified.grants
    );

    // The operator key held the project as its final grant: revoked, it
    // no longer authenticates.
    assert!(
        fx.state.server_keys.verify(&operator_key).is_none(),
        "the original operator key no longer authenticates"
    );
}

/// Trees a partial earlier attempt already removed do not stop the
/// cleanup: absence counts as removed, and the purge still finishes the
/// data and grant cleanup from the record.
#[tokio::test]
async fn purge_finishes_the_data_and_grant_cleanup_after_the_trees_are_gone() {
    let fx = fixture("purge-cleanup-partial");
    let name = "purge-cleanup-partial";
    let (id, operator_principal, operator_key) = create_with_operator(&fx, name).await;

    let archived = archive(&fx, name).await;
    let root = PathBuf::from(archived["root"].as_str().unwrap());

    // Out of band: a partial attempt already took the two trees.
    fs::remove_dir_all(&root).unwrap();
    fs::remove_dir_all(fx.lanes.join(name)).unwrap();

    fx.clock
        .store(BASE_SECONDS + DAY_SECONDS + 1, Ordering::SeqCst);

    let (errored, purged) = purge(&fx, name).await;
    assert!(!errored, "{purged}");
    assert_eq!(purged["project_id"], json!(id), "{purged}");
    assert_eq!(
        purged["project_root"],
        json!(root.display().to_string()),
        "the answer still names the recorded root"
    );
    assert_eq!(purged["ungranted"], json!([operator_principal]), "{purged}");
    assert!(
        purged["keys_revoked"].as_u64().unwrap_or(0) >= 1,
        "{purged}"
    );
    assert!(
        !fx.data.join("projects").join(&id).exists(),
        "the server's project directory is still removed"
    );
    assert!(holders(&fx, &id).is_empty(), "every grant went");
    assert!(
        fx.state.server_keys.verify(&operator_key).is_none(),
        "the operator key no longer authenticates"
    );
}
