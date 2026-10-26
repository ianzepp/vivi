use std::fs::OpenOptions;
use std::sync::atomic::Ordering;

use serde_json::{Value, json};

use super::super::PURGE_DELAY_SECONDS;
use super::super::test_support::{BASE_SECONDS, Fixture, fixture};
use super::purge_project;
use crate::server::AppState;

/// Principals holding a grant on `id`, in store order.
fn holders(state: &AppState, id: &str) -> Vec<String> {
    state
        .server_keys
        .list()
        .into_iter()
        .filter(|p| p.grants.iter().any(|g| g.project_id == id))
        .map(|p| p.principal)
        .collect()
}

/// `purge_project` with `name` repeated as `confirm`; the refusal's
/// message on error.
async fn purge(fx: &Fixture, name: &str) -> Result<Value, String> {
    purge_project(&fx.state, &json!({ "name": name, "confirm": name }))
        .await
        .map_err(|error| error.message)
}

fn project_json(fx: &Fixture) -> std::path::PathBuf {
    fx.data_dir
        .join("projects")
        .join(&fx.project_id)
        .join("project.json")
}

/// One second before the deadline the purge is refused, naming the
/// instant the delay allows, and nothing is removed: the trees, the
/// archive record and every grant stay exactly as they were.
#[tokio::test]
async fn one_second_early_refuses_and_preserves_everything() {
    let fx = fixture("purge-early", true).await;
    fx.seed_archived(true, true);
    let deadline = BASE_SECONDS + PURGE_DELAY_SECONDS;
    fx.clock.store(deadline - 1, Ordering::SeqCst);
    let holders_before = holders(&fx.state, &fx.project_id);

    let error = purge(&fx, &fx.name)
        .await
        .expect_err("one second before the deadline is refused");
    assert!(
        error.contains(&fx.rfc3339(deadline)),
        "the refusal names the allowed instant: {error}"
    );

    assert!(fx.root.exists());
    assert!(fx.lanes_dir.exists());
    assert!(project_json(&fx).exists());
    let meta = fx
        .state
        .projects
        .load_meta_including_retired(&fx.project_id)
        .unwrap();
    assert_eq!(meta.archived.unwrap().at, fx.rfc3339(BASE_SECONDS));
    assert_eq!(holders(&fx.state, &fx.project_id), holders_before);
}

/// At the deadline itself the purge runs: the answer names everything
/// removed, the trees and the server-side record are gone, and every
/// grant went with them (the operator key create_project minted was
/// the only holder, so its keys are revoked).
#[tokio::test]
async fn at_the_deadline_the_purge_runs() {
    let fx = fixture("purge-boundary", true).await;
    fx.seed_archived(true, true);
    let holders_before = holders(&fx.state, &fx.project_id);
    fx.clock
        .store(BASE_SECONDS + PURGE_DELAY_SECONDS, Ordering::SeqCst);

    let out = purge(&fx, &fx.name)
        .await
        .expect("the boundary itself is accepted");
    assert_eq!(out["name"], json!(fx.name), "{out}");
    assert_eq!(out["project_id"], json!(fx.project_id), "{out}");
    assert_eq!(out["project_root"], json!(fx.root), "{out}");
    assert_eq!(out["lanes_dir"], json!(fx.lanes_dir), "{out}");
    assert_eq!(
        out["data_dir"],
        json!(fx.data_dir.join("projects").join(&fx.project_id)),
        "{out}"
    );

    assert!(!fx.root.exists());
    assert!(!fx.lanes_dir.exists());
    assert!(!project_json(&fx).exists());

    assert_eq!(out["ungranted"], json!(holders_before), "{out}");
    // One live key per holder, each revoked with the last grant.
    assert_eq!(out["keys_revoked"], json!(holders_before.len()), "{out}");
}

/// A live pump lock stops the purge before anything is removed: the
/// refusal identifies the lock, and the trees and grants stay put.
#[tokio::test]
async fn a_live_pump_lock_stops_the_purge() {
    let fx = fixture("purge-lock", true).await;
    fx.seed_archived(true, true);
    let home = fx.root.join(".vivi").join("pump");
    std::fs::create_dir_all(&home).unwrap();
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(home.join("lock"))
        .unwrap();
    lock.try_lock().unwrap();
    fx.clock
        .store(BASE_SECONDS + PURGE_DELAY_SECONDS, Ordering::SeqCst);
    let holders_before = holders(&fx.state, &fx.project_id);

    let error = purge(&fx, &fx.name)
        .await
        .expect_err("a held pump lock refuses the purge");
    assert!(
        error.contains(&home.join("lock").display().to_string()),
        "the refusal identifies the lock: {error}"
    );

    assert!(fx.root.exists());
    assert!(fx.lanes_dir.exists());
    assert!(project_json(&fx).exists());
    assert_eq!(holders(&fx.state, &fx.project_id), holders_before);
}

/// Trees a partial earlier attempt already removed do not stop the
/// purge: absence counts as removed, and the server-side record still
/// goes last.
#[tokio::test]
async fn already_removed_trees_still_purge() {
    let fx = fixture("purge-gone", true).await;
    fx.seed_archived(true, true);
    std::fs::remove_dir_all(&fx.root).unwrap();
    std::fs::remove_dir_all(&fx.lanes_dir).unwrap();
    fx.clock
        .store(BASE_SECONDS + PURGE_DELAY_SECONDS, Ordering::SeqCst);

    let out = purge(&fx, &fx.name)
        .await
        .expect("absent trees are already removed");
    assert_eq!(out["project_id"], json!(fx.project_id), "{out}");
    assert!(!project_json(&fx).exists());
}

/// A corrupt archive stamp refuses the purge however old the clock
/// says it is, and nothing is removed.
#[tokio::test]
async fn a_corrupt_stamp_refuses_the_purge() {
    let fx = fixture("purge-corrupt", true).await;
    fx.seed_archived(true, true);
    let mut record = fx
        .state
        .projects
        .load_meta_including_retired(&fx.project_id)
        .unwrap()
        .archived
        .unwrap();
    record.at = "not-a-timestamp".into();
    fx.state
        .projects
        .set_archived(&fx.project_id, Some(record))
        .unwrap();
    fx.clock
        .store(BASE_SECONDS + PURGE_DELAY_SECONDS, Ordering::SeqCst);

    let error = purge(&fx, &fx.name)
        .await
        .expect_err("a corrupt stamp refuses the purge");
    assert!(error.contains("not-a-timestamp"), "{error}");
    assert!(fx.root.exists());
    assert!(fx.lanes_dir.exists());
    assert!(project_json(&fx).exists());
}

/// A project with no archive record is refused before anything moves.
#[tokio::test]
async fn a_live_project_refuses_the_purge() {
    let fx = fixture("purge-live", true).await;
    let holders_before = holders(&fx.state, &fx.project_id);

    let error = purge(&fx, &fx.name)
        .await
        .expect_err("a project that is not archived refuses");
    assert!(error.contains("not archived"), "{error}");
    assert!(fx.root.exists());
    assert!(fx.lanes_dir.exists());
    assert!(project_json(&fx).exists());
    assert_eq!(holders(&fx.state, &fx.project_id), holders_before);
}

/// A confirm that does not repeat the name is refused before any
/// metadata is read.
#[tokio::test]
async fn a_mismatched_confirm_refuses() {
    let fx = fixture("purge-confirm", true).await;
    fx.seed_archived(true, true);

    let error = purge_project(&fx.state, &json!({ "name": fx.name, "confirm": "other" }))
        .await
        .map_err(|error| error.message)
        .expect_err("a mismatched confirm refuses");
    assert!(error.contains("confirmation"), "{error}");
    assert!(fx.root.exists());
    assert!(project_json(&fx).exists());
}
