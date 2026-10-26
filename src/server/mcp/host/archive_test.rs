use std::sync::atomic::Ordering;

use serde_json::json;

use crate::server::grants::{GrantLevel, GrantRecord};
use crate::server::projects::Archived;

use super::super::create_project;
use super::archive_project;
use super::drop_project_grants;
use super::test_support::{BASE_SECONDS, fixture};

fn grant(project_id: &str, member: &str) -> GrantRecord {
    GrantRecord {
        project_id: project_id.to_owned(),
        member: member.to_owned(),
        identity: None,
        level: GrantLevel::Member,
    }
}

/// The principals holding a grant on `id`, in store order.
fn holders(state: &crate::server::AppState, id: &str) -> Vec<String> {
    state
        .server_keys
        .list()
        .into_iter()
        .filter(|p| p.grants.iter().any(|g| g.project_id == id))
        .map(|p| p.principal)
        .collect()
}

/// Every grant on the fixture project comes off, a principal left with
/// no grant at all has its keys revoked, and a principal granted only
/// on another project keeps both its grant and a usable key. Host
/// principals are skipped.
#[tokio::test]
async fn cleanup_ungrants_the_project_and_revokes_only_final_grant_holders() {
    let fx = fixture("grants", true).await;
    let id = fx.project_id.clone();

    // One principal whose only grant is this project (final grant),
    // one granted on another project only, and a host principal.
    let solo = fx
        .state
        .server_keys
        .mint(
            &format!("solo@{id}"),
            false,
            vec![grant(&id, "solo")],
            "solo",
            "test",
        )
        .unwrap();
    let keeper = fx
        .state
        .server_keys
        .mint(
            "keeper@other-project",
            false,
            vec![grant("other-project", "keeper")],
            "keeper",
            "test",
        )
        .unwrap();
    let host_key = fx
        .state
        .server_keys
        .mint("unit-host", true, vec![], "host", "test")
        .unwrap();

    // The operator key `create_project` minted is the second holder.
    let before = holders(&fx.state, &id);
    assert_eq!(
        before.len(),
        2,
        "operator and solo hold the project: {before:?}"
    );

    let (ungranted, revoked) = drop_project_grants(&fx.state, &id)
        .map_err(|e| e.message)
        .unwrap();

    assert_eq!(ungranted, before);
    assert_eq!(revoked, 2, "one live key each: {revoked}");

    // Every holder lost the project's grant, and a final-grant holder
    // lost every key: the raw secret no longer verifies.
    for principal in &ungranted {
        let after = fx
            .state
            .server_keys
            .list()
            .into_iter()
            .find(|p| &p.principal == principal)
            .unwrap();
        assert!(
            after.grants.iter().all(|g| g.project_id != id),
            "{principal} still holds a grant: {:?}",
            after.grants
        );
        assert!(
            after.keys.iter().all(|k| k.revoked_at.is_some()),
            "{principal} kept a live key: {:?}",
            after.keys
        );
    }
    assert!(fx.state.server_keys.verify(&solo.key).is_none());

    // The unrelated principal keeps its grant and a usable key.
    let verified = fx
        .state
        .server_keys
        .verify(&keeper.key)
        .expect("the keeper key still verifies");
    assert_eq!(verified.principal, "keeper@other-project");
    assert!(
        verified
            .grants
            .iter()
            .any(|g| g.project_id == "other-project"),
        "{:?}",
        verified.grants
    );

    // The host principal is untouched.
    assert!(fx.state.server_keys.verify(&host_key.key).is_some());
}

/// A principal that holds another grant besides this project is
/// ungranted on this project but keeps its keys.
#[tokio::test]
async fn cleanup_keeps_a_principal_that_still_holds_another_grant() {
    let fx = fixture("grants-keep", true).await;
    let id = fx.project_id.clone();

    let multi = fx
        .state
        .server_keys
        .mint(
            &format!("multi@{id}"),
            false,
            vec![grant(&id, "multi"), grant("other-project", "multi")],
            "multi",
            "test",
        )
        .unwrap();

    let before = holders(&fx.state, &id);
    let (ungranted, revoked) = drop_project_grants(&fx.state, &id)
        .map_err(|e| e.message)
        .unwrap();

    assert_eq!(ungranted, before);
    assert_eq!(revoked, 1, "only the operator key died: {revoked}");

    let verified = fx
        .state
        .server_keys
        .verify(&multi.key)
        .expect("a principal with another grant keeps its keys");
    assert_eq!(verified.principal, format!("multi@{id}"));
    assert_eq!(
        verified
            .grants
            .iter()
            .map(|g| g.project_id.as_str())
            .collect::<Vec<_>>(),
        vec!["other-project"]
    );
}

/// A cleanup interrupted after its first write (the final-grant holder's
/// keys revoked, its grant not yet removed) leaves an inert grant, and
/// the retry still finds that principal, removes the grant and keeps
/// the keys revoked: no interruption leaves live keys behind.
#[tokio::test]
async fn an_interrupted_cleanup_is_finished_by_the_retry_and_never_fails_open() {
    let fx = fixture("grants-retry", true).await;
    let id = fx.project_id.clone();

    let solo = fx
        .state
        .server_keys
        .mint(
            &format!("solo@{id}"),
            false,
            vec![grant(&id, "solo")],
            "solo",
            "test",
        )
        .unwrap();
    let principal = format!("solo@{id}");

    // The first write of the cleanup, then the interruption.
    fx.state.server_keys.revoke_principal(&principal).unwrap();
    assert!(
        holders(&fx.state, &id).contains(&principal),
        "the grant is still there for the retry to find"
    );
    assert!(
        fx.state.server_keys.verify(&solo.key).is_none(),
        "the leftover grant is inert: its key no longer verifies"
    );

    let (ungranted, _) = drop_project_grants(&fx.state, &id)
        .map_err(|e| e.message)
        .unwrap();

    assert!(ungranted.contains(&principal), "{ungranted:?}");
    assert!(holders(&fx.state, &id).is_empty());
    assert!(fx.state.server_keys.verify(&solo.key).is_none());
}

/// The fixture creates a project through the real handler and
/// `seed_archived` stands in for the archive handler: the record is
/// stamped with the fixture clock, the root is unregistered, the
/// project is retired, and the name stays taken.
#[tokio::test]
async fn fixture_creates_and_seeds_an_archived_project() {
    let fx = fixture("seed", true).await;

    assert!(
        fx.data_dir
            .join("projects")
            .join(&fx.project_id)
            .join("project.json")
            .exists()
    );
    assert_eq!(
        std::fs::canonicalize(&fx.root).unwrap(),
        std::fs::canonicalize(fx.state.host.projects_root.clone().unwrap().join(&fx.name)).unwrap()
    );
    assert!(!fx.state.projects.is_retired(&fx.project_id));

    fx.seed_archived(true, false);

    let meta = fx
        .state
        .projects
        .load_meta_including_retired(&fx.project_id)
        .unwrap();
    assert!(meta.is_archived());
    let record = meta.archived.unwrap();
    assert_eq!(record.at, fx.rfc3339(BASE_SECONDS));
    assert_eq!(record.by, "mind");
    assert_eq!(record.root, fx.root);
    assert_eq!(record.lanes_dir, fx.lanes_dir);
    assert!(record.pump);
    assert!(!record.serve);
    assert!(fx.state.projects.is_retired(&fx.project_id));

    // The archived name cannot be created again.
    let again = create_project(
        &fx.state,
        &serde_json::json!({"name": fx.name, "initial_commit": true, "serve": true}),
    )
    .await;
    assert_eq!(
        again.err().map(|e| e.message),
        Some(format!(
            "{} is archived: its name cannot be reused",
            fx.name
        ))
    );
}

/// Archiving succeeds in one call while a pump still holds the real
/// `.vivi/pump/lock`, and every tree is left exactly as it was. The
/// record carries the exact registry flags, the caller and the fixture
/// clock, and the root is unregistered.
#[tokio::test]
async fn archive_succeeds_under_a_held_pump_lock_and_keeps_every_file() {
    let fx = fixture("archive", true).await;

    // A pump mid-tick: a held lock in the mailspace.
    let pump_dir = fx.root.join(".vivi").join("pump");
    std::fs::create_dir_all(&pump_dir).unwrap();
    std::fs::write(pump_dir.join("lock"), b"held").unwrap();

    // Sentinels in the three trees archive must never touch: the
    // mailspace root, the lanes directory, the server's data directory.
    std::fs::write(fx.root.join("sentinel.txt"), b"mailspace").unwrap();
    std::fs::write(fx.lanes_dir.join("sentinel.txt"), b"lanes").unwrap();
    let project_data = fx.data_dir.join("projects").join(&fx.project_id);
    std::fs::write(project_data.join("sentinel.txt"), b"data").unwrap();

    let made = archive_project(
        &fx.state,
        "mind",
        &json!({
            "name": fx.name,
            "confirm": fx.name,
        }),
    )
    .await
    .map_err(|e| e.message)
    .expect("the archive succeeds");

    assert_eq!(made["name"], json!(fx.name));
    assert_eq!(made["project_id"], json!(fx.project_id));
    assert_eq!(made["unregistered"], json!(true));
    let record: Archived = serde_json::from_value(made["archived"].clone()).unwrap();
    assert_eq!(record.at, fx.rfc3339(BASE_SECONDS));
    assert_eq!(record.by, "mind");
    assert_eq!(record.root, fx.root);
    assert_eq!(record.lanes_dir, fx.lanes_dir);
    assert!(record.pump, "created with pump:true: {:?}", record);
    assert!(record.serve, "created with serve:true: {:?}", record);

    // Every file in place: the held lock, the three sentinels, the
    // project's metadata.
    assert!(pump_dir.join("lock").exists());
    assert!(fx.root.join("sentinel.txt").exists());
    assert!(fx.lanes_dir.join("sentinel.txt").exists());
    assert!(project_data.join("sentinel.txt").exists());
    assert!(project_data.join("project.json").exists());

    // Retired, and out of the host registry.
    assert!(fx.state.projects.is_retired(&fx.project_id));
    assert!(crate::registry::load().unwrap().get(&fx.root).is_none());
}

/// A retry preserves the first record field for field — `at` included,
/// however far the clock has moved — and only brings the registry in
/// line (a no-op the second time).
#[tokio::test]
async fn retry_preserves_the_first_archive_stamp() {
    let fx = fixture("retry", true).await;

    let first = archive_project(
        &fx.state,
        "mind",
        &json!({
            "name": fx.name,
            "confirm": fx.name,
        }),
    )
    .await
    .map_err(|e| e.message)
    .expect("the first archive succeeds");
    fx.clock.store(BASE_SECONDS + 3_600, Ordering::SeqCst);
    let second = archive_project(
        &fx.state,
        "hand",
        &json!({
            "name": fx.name,
            "confirm": fx.name,
        }),
    )
    .await
    .map_err(|e| e.message)
    .expect("the retry succeeds");

    let a: Archived = serde_json::from_value(first["archived"].clone()).unwrap();
    let b: Archived = serde_json::from_value(second["archived"].clone()).unwrap();
    assert_eq!(a, b, "the retry returns the first record unchanged");
    assert_eq!(b.at, fx.rfc3339(BASE_SECONDS));
    assert_eq!(b.by, "mind");
    assert_eq!(second["unregistered"], json!(false), "already gone");
    let meta = fx
        .state
        .projects
        .load_meta_including_retired(&fx.project_id)
        .unwrap();
    assert_eq!(meta.archived, Some(b));
}

/// A confirmation that does not repeat the name refuses before any
/// mutation: the project is not archived, not retired, still served.
#[tokio::test]
async fn wrong_confirmation_leaves_the_project_untouched() {
    let fx = fixture("confirm", true).await;

    let refused = archive_project(
        &fx.state,
        "mind",
        &json!({
            "name": fx.name,
            "confirm": "something-else",
        }),
    )
    .await;

    assert!(refused.is_err());
    let meta = fx
        .state
        .projects
        .load_meta_including_retired(&fx.project_id)
        .unwrap();
    assert!(meta.archived.is_none());
    assert!(!fx.state.projects.is_retired(&fx.project_id));
    assert!(crate::registry::load().unwrap().get(&fx.root).is_some());
}

/// A name this server does not host (a registry-only root has no
/// server `project.json`, so it never resolves) is refused.
#[tokio::test]
async fn a_name_this_server_does_not_host_is_refused() {
    let fx = fixture("unknown", true).await;

    let refused = archive_project(
        &fx.state,
        "mind",
        &json!({
            "name": "no-such-project-here",
            "confirm": "no-such-project-here",
        }),
    )
    .await
    .map_err(|e| e.message);

    assert_eq!(
        refused,
        Err(String::from(
            "no project \"no-such-project-here\" is hosted by this server"
        ))
    );
    assert!(!fx.state.projects.is_retired(&fx.project_id));
}

/// Archive needs only the lanes configuration: `HOST_LANES_ROOT`
/// canonicalized and joined with the name is the recorded `lanes_dir`,
/// while the root and its flags come from the registry.
#[tokio::test]
async fn archive_works_with_only_the_lanes_configuration() {
    let mut fx = fixture("lanes-only", true).await;
    fx.state.host = super::super::HostConfig {
        lanes_root: fx.state.host.lanes_root.clone(),
        ..Default::default()
    };

    let made = archive_project(
        &fx.state,
        "mind",
        &json!({
            "name": fx.name,
            "confirm": fx.name,
        }),
    )
    .await
    .map_err(|e| e.message)
    .expect("the archive succeeds with only HOST_LANES_ROOT set");

    let record: Archived = serde_json::from_value(made["archived"].clone()).unwrap();
    assert_eq!(record.lanes_dir, fx.lanes_dir);
    assert_eq!(record.root, fx.root);
    assert!(record.pump && record.serve);
}

/// A root absent from a readable registry is recorded as the project's
/// `mailspace_root` with explicit false/false flags (never registry
/// defaults), and this call's update removes nothing. The fixture
/// mutation that empties the entry is serialized under `CREATING`, the
/// same mutex every registry mutation in this process holds.
#[tokio::test]
async fn a_root_absent_from_the_registry_records_false_flags() {
    let fx = fixture("absent", true).await;
    {
        let _one = super::super::CREATING
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        crate::registry::update(|registry| Ok(registry.remove(&fx.root)))
            .map_err(|e| e.to_string())
            .expect("the root is unregistered");
    }

    let made = archive_project(
        &fx.state,
        "mind",
        &json!({
            "name": fx.name,
            "confirm": fx.name,
        }),
    )
    .await
    .map_err(|e| e.message)
    .expect("the archive succeeds without a registry entry");

    assert_eq!(made["unregistered"], json!(false));
    let record: Archived = serde_json::from_value(made["archived"].clone()).unwrap();
    assert_eq!(record.root, fx.root, "the mailspace root is recorded");
    assert!(!record.pump && !record.serve, "explicit false/false");
    assert_eq!(record.lanes_dir, fx.lanes_dir);
}

/// The purge delay is one day, by policy: no override exists.
#[test]
fn purge_delay_is_one_day() {
    assert_eq!(super::PURGE_DELAY_SECONDS, 24 * 60 * 60);
}
