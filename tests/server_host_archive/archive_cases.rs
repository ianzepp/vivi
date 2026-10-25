//! Archive cases (`tests/host_archive.rs` child module): a served project
//! is archived through the archive tool — every file stays exactly where
//! it lay (a held pump lock does not matter), the record carries the
//! fixture clock, the host actor and the registered flags, the root leaves
//! the host registry at once, and the archived name is refused by the
//! project tools, `create_project` and `lane_create` until a restore. A
//! bad confirmation, and a registry-only root the server never served,
//! touch nothing.
//!
//! Clock discipline: the only time that moves is `Fx::clock`
//! (`Arc<AtomicI64>`); no archive stamp in metadata is ever rewritten and
//! the production delay is never touched.

use std::fs::{self, OpenOptions};
use std::path::PathBuf;
use std::sync::atomic::Ordering;

use serde_json::{Value, json};
use vivi::registry::{self, Flags};
use vivi_server::mcp::host::HostConfig;

use super::{BASE_SECONDS, Fx, fixture, rfc3339, router, snapshot};

/// One hour, for the clock advance before the archive retry.
const ONE_HOUR: i64 = 60 * 60;

/// Create one served project (`initial_commit`) and return `(project id,
/// mailspace root, minted operator key)`.
async fn created(fx: &Fx, name: &str) -> (String, PathBuf, String) {
    let (errored, made) = fx
        .create(json!({ "name": name, "initial_commit": true }))
        .await;
    assert!(!errored, "{made}");
    assert_eq!(made["served"], true, "{made}");
    let id = made["project_id"].as_str().unwrap().to_owned();
    let root: PathBuf = made["project_root"].as_str().unwrap().to_owned().into();
    let key = made["operator_key"]["key"].as_str().unwrap().to_owned();
    (id, root, key)
}

/// Archive `name` with a matching confirm, required to succeed.
async fn archive(fx: &Fx, name: &str) -> Value {
    let (errored, archived) = fx
        .call_tool("archive_project", json!({ "name": name, "confirm": name }))
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

/// An archive succeeds in one call while the project's real pump lock is
/// held: the mailspace root, the lanes root and every pre-existing file
/// under `DATA_DIR/projects/<id>` except `project.json` are retained at
/// their original sizes, the record stamps the fixture clock, the host
/// actor, the exact paths and the registered flags, the root is
/// unregistered at once, `project_list` reports the project archived, and
/// a retry far later preserves the record and its timestamp.
#[tokio::test]
async fn archive_keeps_every_file_and_unregisters_without_waiting_for_the_pump_lock() {
    let fx = fixture("archive-keep");
    let name = "archive-keep";
    let (id, root, key) = created(&fx, name).await;

    // A pump mid-tick: the real `.vivi/pump/lock`, held by this test.
    let lock = fx.hold_pump_lock(name);

    // What lies beneath the mailspace root, the lanes root and the
    // server's own project directory before anything is archived.
    let root_tree = snapshot(&root);
    let lanes_tree = snapshot(&fx.lanes);
    let data_tree = snapshot(&fx.data.join("projects").join(&id));
    assert!(
        root_tree.iter().any(|(path, _)| path == ".vivi/pump/lock"),
        "the held lock is part of the snapshot: {root_tree:?}"
    );

    let archived = archive(&fx, name).await;
    assert_eq!(archived["name"], json!(name), "{archived}");
    assert_eq!(archived["project_id"], json!(id), "{archived}");
    assert_eq!(archived["unregistered"], json!(true), "{archived}");
    assert_eq!(
        archived["root"],
        json!(root.display().to_string()),
        "{archived}"
    );
    assert!(
        registry::load().unwrap().get(&root).is_none(),
        "the archived root is out of the host registry"
    );

    // Every pre-existing file is retained at its size, project.json alone
    // excepted: the archive record is stamped onto it.
    assert_eq!(snapshot(&root), root_tree, "the mailspace root");
    assert_eq!(snapshot(&fx.lanes), lanes_tree, "the lanes root");
    let after = snapshot(&fx.data.join("projects").join(&id));
    assert_eq!(
        after.iter().map(|(path, _)| path).collect::<Vec<_>>(),
        data_tree.iter().map(|(path, _)| path).collect::<Vec<_>>(),
        "the same file set under DATA_DIR/projects/<id>"
    );
    for (path, size) in &data_tree {
        if path != "project.json" {
            assert_eq!(
                after.iter().find(|(kept, _)| kept == path).unwrap().1,
                *size,
                "{path} is retained at its original size"
            );
        }
    }

    // The stamped record: the fixture clock, the host actor, the exact
    // paths and the flags creation registered.
    let record = fx.project_json(&id)["archived"].clone();
    assert_eq!(record["at"], json!(rfc3339(BASE_SECONDS)), "{record}");
    assert_eq!(record["by"], json!("host"), "{record}");
    assert_eq!(
        record["root"],
        json!(root.display().to_string()),
        "{record}"
    );
    assert_eq!(
        record["lanes_dir"],
        json!(
            fx.lanes
                .canonicalize()
                .unwrap()
                .join(name)
                .display()
                .to_string()
        ),
        "{record}"
    );
    assert_eq!(record["pump"], json!(true), "{record}");
    assert_eq!(record["serve"], json!(true), "{record}");
    assert_eq!(archived["archived"], record, "the answer is the record");

    // The host's project_list reports the project archived.
    let entry = listed_entry(&fx, &id).await;
    assert_eq!(entry["mode"], "archived", "{entry}");
    assert_eq!(entry["name"], json!(name), "{entry}");

    // The lock was never waited on and never taken: still held.
    let probe = OpenOptions::new()
        .write(true)
        .open(fx.projects.join(name).join(".vivi/pump/lock"))
        .unwrap();
    assert!(probe.try_lock().is_err(), "the pump lock is still held");
    // The minted operator key survives with its grant intact.
    let verified = fx
        .state
        .server_keys
        .verify(&key)
        .expect("the operator key survives the archive");
    assert!(
        verified.grants.iter().any(|grant| grant.project_id == id),
        "{:?}",
        verified.grants
    );

    // A retry far later preserves the record and its timestamp, and the
    // registry update is a no-op.
    fx.clock.store(BASE_SECONDS + ONE_HOUR, Ordering::SeqCst);
    let again = archive(&fx, name).await;
    assert_eq!(again["unregistered"], json!(false), "{again}");
    assert_eq!(again["archived"], record, "{again}");
    assert_eq!(
        fx.project_json(&id)["archived"],
        record,
        "the stamped record is unchanged"
    );
    drop(lock);
}

/// After the archive, the original operator key is refused on every
/// project tool — read and write, by project id and by hosted name — with
/// the archived project named, while `projects` still lists its grant with
/// mode archived. `create_project` and `lane_create` refuse the archived
/// name without adding a root, lane, checkout, or config backup/change,
/// and the key and its grant are retained.
#[tokio::test]
async fn project_tools_refuse_an_archived_project_and_create_refuses_its_name() {
    let fx = fixture("archive-refuse");
    let name = "archive-refuse";
    let (id, root, key) = created(&fx, name).await;
    archive(&fx, name).await;

    // Read and write project tools refuse by id and by hosted name, each
    // naming the archived project (never the raw name argument).
    for named in [id.as_str(), name] {
        for (tool, mut arguments) in [
            ("pump_list", json!({})),
            ("memo", json!({ "action": "save" })),
        ] {
            arguments["project"] = json!(named);
            let (errored, refused) = fx.call_as(&key, tool, arguments).await;
            assert!(errored, "{tool} by {named}: {refused}");
            let message = refused["error"].as_str().unwrap();
            assert!(message.contains("archived"), "{tool} by {named}: {message}");
            assert!(
                message.contains(&id),
                "{tool} by {named}: the refusal names the project id: {message}"
            );
        }
    }

    // The grant still resolves: the key's own listing reports it archived.
    let (errored, listed) = fx.call_as(&key, "projects", json!({})).await;
    assert!(!errored, "{listed}");
    let entry = listed
        .as_array()
        .expect("projects answers a list")
        .iter()
        .find(|item| item["project_id"] == json!(id))
        .unwrap_or_else(|| panic!("the grant is listed: {listed:?}"))
        .clone();
    assert_eq!(entry["mode"], "archived", "{entry}");

    // The refusal baseline: no new root, lane, checkout, or config
    // backup/change may appear.
    let root_tree = snapshot(&fx.projects);
    let lanes_tree = snapshot(&fx.lanes);
    let data_tree = snapshot(&fx.data.join("projects").join(&id));

    let (errored, again) = fx
        .create(json!({ "name": name, "initial_commit": true }))
        .await;
    assert!(errored, "{again}");
    assert!(
        again["error"]
            .as_str()
            .unwrap()
            .contains("is archived: its name cannot be reused"),
        "{again}"
    );

    let (errored, lane) = fx
        .call_tool(
            "lane_create",
            json!({ "name": name, "target": "second-lane" }),
        )
        .await;
    assert!(errored, "{lane}");
    assert!(
        lane["error"]
            .as_str()
            .unwrap()
            .contains("is archived: its name cannot take a new lane"),
        "{lane}"
    );

    assert_eq!(snapshot(&fx.projects), root_tree, "no new root or checkout");
    assert_eq!(snapshot(&fx.lanes), lanes_tree, "no new lane");
    assert_eq!(
        snapshot(&fx.data.join("projects").join(&id)),
        data_tree,
        "no config backup or change"
    );
    assert!(
        registry::load().unwrap().get(&root).is_none(),
        "the archived root stays out of the registry"
    );

    // The original key and its grant were retained.
    let verified = fx
        .state
        .server_keys
        .verify(&key)
        .expect("the original key is retained");
    assert!(
        verified.grants.iter().any(|grant| grant.project_id == id),
        "{:?}",
        verified.grants
    );
}

/// An archive whose confirm does not repeat the name refuses before any
/// mutation: the project metadata, all three trees, this root's registry
/// membership and the hosted listing are unchanged.
#[tokio::test]
async fn a_bad_confirmation_refuses_and_touches_nothing() {
    let fx = fixture("archive-confirm");
    let name = "archive-confirm";
    let (id, root, _key) = created(&fx, name).await;

    let meta_before = fx.project_json(&id);
    let root_tree = snapshot(&fx.projects);
    let lanes_tree = snapshot(&fx.lanes);
    let data_tree = snapshot(&fx.data.join("projects").join(&id));
    let registered = registry::load().unwrap().get(&root);
    assert_eq!(
        registered,
        Some(Flags {
            pump: true,
            serve: true
        })
    );

    let (errored, refused) = fx
        .call_tool(
            "archive_project",
            json!({ "name": name, "confirm": "something-else" }),
        )
        .await;
    assert!(errored, "{refused}");
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .contains("needs a confirmation that repeats its name"),
        "{refused}"
    );

    assert_eq!(fx.project_json(&id), meta_before, "metadata unchanged");
    assert_eq!(snapshot(&fx.projects), root_tree, "the mailspace root");
    assert_eq!(snapshot(&fx.lanes), lanes_tree, "the lanes root");
    assert_eq!(
        snapshot(&fx.data.join("projects").join(&id)),
        data_tree,
        "the server's project directory"
    );
    assert_eq!(
        registry::load().unwrap().get(&root),
        registered,
        "this root's registry membership is unchanged"
    );
    assert_eq!(listed_entry(&fx, &id).await["mode"], "hosted");
}

/// A project the registry knows but this server never served
/// (`create_project` with `serve: false`) is registry-only: archiving it
/// refuses as not hosted, and the registry entry and the server's own
/// store are untouched.
#[tokio::test]
async fn a_registry_only_project_is_refused_as_not_hosted() {
    let fx = fixture("registry-only");
    let name = "registry-only-proj";
    let (errored, made) = fx
        .create(json!({ "name": name, "initial_commit": true, "serve": false }))
        .await;
    assert!(!errored, "{made}");
    assert_eq!(made["served"], false, "{made}");
    assert!(
        made.get("project_id").is_none() && made.get("operator_key").is_none(),
        "nothing was served, so no id or key was minted: {made}"
    );

    let root = fx.projects.join(name).canonicalize().unwrap();
    let registered = registry::load().unwrap().get(&root);
    assert_eq!(
        registered,
        Some(Flags {
            pump: true,
            serve: false
        }),
        "registry-only: the registry knows the root"
    );
    // The server store has no metadata for an unserved project: make sure
    // the directory exists so the before/after snapshot is honest.
    fs::create_dir_all(fx.data.join("projects")).unwrap();
    let data_tree = snapshot(&fx.data.join("projects"));

    let (errored, refused) = fx
        .call_tool("archive_project", json!({ "name": name, "confirm": name }))
        .await;
    assert!(errored, "{refused}");
    let message = refused["error"].as_str().unwrap();
    assert!(message.contains("is hosted by this server"), "{message}");
    assert!(message.contains(name), "{message}");

    assert_eq!(
        registry::load().unwrap().get(&root),
        registered,
        "the registry entry is untouched"
    );
    assert_eq!(
        snapshot(&fx.data.join("projects")),
        data_tree,
        "the server's project store is untouched"
    );
    let (errored, listed) = fx.call_tool("project_list", json!({})).await;
    assert!(!errored, "{listed}");
    assert!(
        listed
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["name"] != json!(name)),
        "project_list knows no such project: {listed:?}"
    );
}

/// After creation the archive needs only `HOST_LANES_ROOT`: with every
/// other host setting blanked the tool still succeeds and records the
/// lanes directory under it.
#[tokio::test]
async fn archive_after_creation_needs_only_the_lanes_configuration() {
    let mut fx = fixture("lanes-only");
    let name = "lanes-only-proj";
    let (id, root, _key) = created(&fx, name).await;

    fx.state.host = HostConfig {
        lanes_root: Some(fx.lanes.clone()),
        ..HostConfig::default()
    };
    fx.app = router(fx.state.clone());

    let archived = archive(&fx, name).await;
    assert_eq!(archived["unregistered"], json!(true), "{archived}");
    let record = &archived["archived"];
    assert_eq!(record["at"], json!(rfc3339(BASE_SECONDS)), "{archived}");
    assert_eq!(
        record["root"],
        json!(root.display().to_string()),
        "{archived}"
    );
    assert_eq!(
        record["lanes_dir"],
        json!(
            fx.lanes
                .canonicalize()
                .unwrap()
                .join(name)
                .display()
                .to_string()
        ),
        "{archived}"
    );
    assert_eq!(record["pump"], json!(true), "{archived}");
    assert_eq!(record["serve"], json!(true), "{archived}");
    assert!(
        registry::load().unwrap().get(&root).is_none(),
        "the root is unregistered"
    );
    assert_eq!(
        fx.project_json(&id)["archived"]["by"],
        json!("host"),
        "the host actor is stamped"
    );
}
