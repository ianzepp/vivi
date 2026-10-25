//! Purging an archived project for good: once
//! [`PURGE_DELAY_SECONDS`](super::PURGE_DELAY_SECONDS) have passed since the
//! archive stamp, remove the project's mailspace root and lanes directory,
//! drop every grant on it, and remove the server's own project directory.
//! The steps are ordered so a partially failed purge retries cleanly: the
//! recorded root leaves the host registry first, the trees go next (an
//! already-removed tree counts as removed), the grants are dropped after
//! them, and the server-side directory — the archive record a retry needs
//! to know where the trees were — is removed last. Purging never derives
//! paths from the current `HostConfig`: everything comes from the archive
//! record, so a purge of a project whose name the host now spells
//! differently still removes what was archived.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};
use vivi::project_create::check_name;
use vivi::pump::control::project_home;
use vivi::pump::run::lock_is_held;

use crate::AppState;

use super::super::super::tools::{ToolError, map_project, req_str};
use super::super::{CREATING, blocking, hosted_entry};

/// Purge one archived project. Not wired into the MCP dispatch by this
/// slice; a later host slice exposes it.
pub(crate) async fn purge_project(state: &AppState, arguments: &Value) -> Result<Value, ToolError> {
    let name = req_str(arguments, "name")?.to_owned();
    let confirm = req_str(arguments, "confirm")?.to_owned();
    if confirm != name {
        return Err(ToolError::new(format!(
            "purging project {name:?} needs a confirmation that repeats its name"
        )));
    }
    check_name(&name).map_err(|err| ToolError::new(err.to_string()))?;
    let state = state.clone();
    blocking(move || {
        // One host-project mutation at a time, the same mutex
        // `create_project` and `delete_project` hold.
        let _one = CREATING.lock().unwrap_or_else(|e| e.into_inner());
        purge_locked(&state, &name)
    })
    .await
}

/// Everything purge does, under the host mutation mutex.
fn purge_locked(state: &AppState, name: &str) -> Result<Value, ToolError> {
    let meta = hosted_entry(state, name)
        .ok_or_else(|| ToolError::new(format!("no project {name:?} on this host")))?;
    let Some(record) = meta.archived.clone() else {
        return Err(ToolError::new(format!("project {name} is not archived")));
    };

    // The ruling: purging requires at least 24 hours after archive, for
    // every caller. The elapsed time reads only the store's own clock.
    let archived_at = DateTime::parse_from_rfc3339(&record.at)
        .map_err(|error| {
            ToolError::new(format!(
                "the archive stamp {:?} on project {} is corrupt: {error}",
                record.at, name
            ))
        })?
        .with_timezone(&Utc);
    let allowed_from = archived_at + chrono::Duration::seconds(super::PURGE_DELAY_SECONDS);
    if state.projects.now() < allowed_from {
        return Err(ToolError::new(format!(
            "project {name} was archived at {}; purge is allowed from {} (24 hours after the \
             archive)",
            record.at,
            allowed_from.to_rfc3339_opts(SecondsFormat::Secs, true)
        )));
    }

    // The recorded root leaves the registry (again: the archive already
    // unregistered it), so nothing serves the mailspace while it goes.
    vivi::registry::update(|registry| Ok(registry.remove(&record.root)))
        .map_err(|error| ToolError::new(format!("could not update the host registry: {error}")))?;
    crate::hosted::reconcile_registry(&state.projects, &state.identities);

    // A live pump may be mid-tick or mid-job; removing its files under it
    // would break that run. A root without a resolvable pump configuration
    // (already gone, or never a pump project) has no lock to hold.
    if let Ok(home) = project_home(&record.root) {
        let held = lock_is_held(&home).map_err(|error| {
            ToolError::new(format!(
                "could not inspect the pump lock at {}: {error}",
                home.display()
            ))
        })?;
        if held {
            return Err(ToolError::new(format!(
                "a pump still holds the lock at {}; stop the pump service and call purge again",
                home.join("lock").display()
            )));
        }
    }

    remove_tree(&record.root, "project root")?;
    remove_tree(&record.lanes_dir, "lanes directory")?;
    let (ungranted, revoked) = super::drop_project_grants(state, &meta.project_id)?;

    // LAST: this directory holds the archive record a partially failed
    // purge needs to retry, so it only goes once nothing else is left.
    let data_dir = state
        .projects
        .project_dir(&meta.project_id)
        .map_err(map_project)?;
    remove_tree(&data_dir, "project data directory")?;

    Ok(json!({
        "name": name,
        "project_id": meta.project_id,
        "project_root": record.root,
        "lanes_dir": record.lanes_dir,
        "data_dir": data_dir,
        "ungranted": ungranted,
        "keys_revoked": revoked,
    }))
}

/// Remove a whole tree; absent already counts as removed, so a purge that
/// failed part-way through can simply run again.
fn remove_tree(path: &Path, what: &str) -> Result<(), ToolError> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ToolError::new(format!(
            "could not remove the {what} {}: {error}",
            path.display()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::sync::atomic::Ordering;

    use serde_json::{Value, json};

    use super::super::PURGE_DELAY_SECONDS;
    use super::super::test_support::{BASE_SECONDS, Fixture, fixture};
    use super::purge_project;
    use crate::AppState;

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
}
