//! Host archive lifecycle support: the fixed purge delay, the grant
//! cleanup every lifecycle step shares, and the `archive_project` handler
//! that stamps an archive record and unregisters the root without touching
//! a single file. The `restore` and `purge` child modules hold their
//! implementations (separate slices); no MCP tool is exposed from here
//! yet — the catalog lands on this foundation.

/// How long an archived project waits between its archive stamp and the
/// moment `purge` may remove its files: 24 hours, fixed. There is no
/// environment override and no other production delay definition on
/// purpose: the delay is policy, not configuration.
pub(super) const PURGE_DELAY_SECONDS: i64 = 24 * 60 * 60;

pub(super) mod purge;
pub(super) mod restore;

#[cfg(test)]
mod test_support;

use chrono::SecondsFormat;
use serde_json::{Value, json};
use vivi::registry::Flags;

use super::super::tools::{ToolError, map_project, req_str};
use crate::AppState;
use crate::projects::Archived;

/// Archive one hosted project. A state-only retirement: stamp the archive
/// record onto the project's metadata, take its mailspace root out of the
/// host registry, and leave every file exactly where it lies — a pump may
/// still hold `.vivi/pump/lock` mid-tick, and archive succeeds anyway.
///
/// Requires `name`, a `confirm` that repeats it and a name that passes
/// [`vivi::project_create::check_name`] before anything is touched, then
/// resolves the project through [`hosted_entry`](super::hosted_entry): a
/// registry-only root with no server `project.json` is not hosted by this
/// server and is refused.
///
/// When the project already carries an archive record (a retry), every
/// field of it is preserved — `at` included — the recorded root is
/// unregistered if still present, the registry reconciles, and the
/// existing record is returned.
///
/// A first archive needs only `HOST_LANES_ROOT`: it is canonicalized and
/// its `join(name)` recorded as `lanes_dir`. The registry is loaded before
/// any metadata write and an unreadable registry aborts. The matching
/// registered root and its exact pump/serve flags are recorded; a root
/// absent from a readable registry is recorded as `mailspace_root` with
/// explicit `false`/`false` (never registry defaults). The record is
/// written first (`set_archived`), then the registry update, then the
/// reconcile. Pump control is never consulted and no file is ever
/// deleted; purge (its own slice) is what removes files once
/// [`PURGE_DELAY_SECONDS`](PURGE_DELAY_SECONDS) have passed.
pub(crate) async fn archive_project(
    state: &AppState,
    by: &str,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let name = req_str(arguments, "name")?.to_owned();
    let confirm = req_str(arguments, "confirm")?.to_owned();
    vivi::project_create::check_name(&name).map_err(|e| ToolError::new(e.to_string()))?;
    if confirm != name {
        return Err(ToolError::new(format!(
            "archiving project {name:?} needs a confirmation that repeats its name"
        )));
    }
    let state = state.clone();
    let by = by.to_owned();
    super::blocking(move || {
        let _one = super::CREATING.lock().unwrap_or_else(|e| e.into_inner());
        let Some(meta) = super::hosted_entry(&state, &name) else {
            return Err(ToolError::new(format!(
                "no project {name:?} is hosted by this server"
            )));
        };
        let project_id = meta.project_id.clone();
        if let Some(existing) = meta.archived.clone() {
            // A retry: the first stamp stands, only the registry is
            // brought in line with the recorded root.
            let unregistered =
                vivi::registry::update(|registry| Ok(registry.remove(&existing.root))).map_err(
                    |e| ToolError::new(format!("the host registry could not be updated: {e}")),
                )?;
            crate::hosted::reconcile_registry(&state.projects, &state.identities);
            return Ok(answer(
                &name,
                &project_id,
                &existing.root,
                &existing.lanes_dir,
                unregistered,
                &existing,
            ));
        }

        let lanes_root = super::require_lanes(&state.host, "archive_project")?;
        let lanes_dir = lanes_root
            .canonicalize()
            .map_err(|e| {
                ToolError::new(format!("could not resolve {}: {e}", super::ENV_LANES_ROOT))
            })?
            .join(&name);
        // Read the registry before any metadata write: a registry that
        // cannot be read aborts the archive with nothing changed.
        let registry = vivi::registry::load()
            .map_err(|e| ToolError::new(format!("the host registry could not be read: {e}")))?;
        let mailspace_root = meta
            .mailspace_root
            .clone()
            .ok_or_else(|| ToolError::new(format!("project {project_id} has no mailspace root")))?;
        let (root, flags) = registry
            .entries()
            .into_iter()
            .find(|entry| super::same_dir(&entry.root, &mailspace_root))
            .map_or_else(
                || {
                    (
                        mailspace_root.clone(),
                        Flags {
                            pump: false,
                            serve: false,
                        },
                    )
                },
                |entry| (entry.root, entry.flags),
            );
        let archived = Archived {
            at: state
                .projects
                .now()
                .to_rfc3339_opts(SecondsFormat::Secs, true),
            by,
            root: root.clone(),
            lanes_dir: lanes_dir.clone(),
            pump: flags.pump,
            serve: flags.serve,
        };
        state
            .projects
            .set_archived(&project_id, Some(archived.clone()))
            .map_err(map_project)?;
        let unregistered = vivi::registry::update(|registry| Ok(registry.remove(&root)))
            .map_err(|e| ToolError::new(format!("the host registry could not be updated: {e}")))?;
        crate::hosted::reconcile_registry(&state.projects, &state.identities);
        Ok(answer(
            &name,
            &project_id,
            &root,
            &lanes_dir,
            unregistered,
            &archived,
        ))
    })
    .await
}

/// The archive answer: what was recorded and whether this call's registry
/// update removed the root.
fn answer(
    name: &str,
    project_id: &str,
    root: &std::path::Path,
    lanes_dir: &std::path::Path,
    unregistered: bool,
    archived: &Archived,
) -> Value {
    json!({
        "name": name,
        "project_id": project_id,
        "root": root,
        "lanes_dir": lanes_dir,
        "unregistered": unregistered,
        "archived": archived,
    })
}

/// Take every grant on `project_id` off its non-host principals, and revoke
/// the keys of each principal left with no grant (such as the operator key
/// `create_project` minted). Returns `(principals ungranted, keys revoked)`.
/// Host principals are skipped: they hold no project grants by rule. A
/// failed grant removal or revocation propagates instead of being skipped,
/// so a caller never reports a cleanup that did not happen.
pub(super) fn drop_project_grants(
    state: &AppState,
    project_id: &str,
) -> Result<(Vec<String>, usize), ToolError> {
    let mut ungranted = Vec::new();
    let mut revoked = 0;
    for principal in state.server_keys.list() {
        if principal.host || !principal.grants.iter().any(|g| g.project_id == project_id) {
            continue;
        }
        state
            .server_keys
            .remove_grant(&principal.principal, project_id)
            .map_err(|e| {
                ToolError::new(format!(
                    "could not remove the grant on {project_id} from {}: {e:?}",
                    principal.principal
                ))
            })?;
        if principal.grants.len() == 1 {
            revoked += state
                .server_keys
                .revoke_principal(&principal.principal)
                .map_err(|e| {
                    ToolError::new(format!(
                        "could not revoke the keys of {}: {e:?}",
                        principal.principal
                    ))
                })?;
        }
        ungranted.push(principal.principal);
    }
    Ok((ungranted, revoked))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use serde_json::json;

    use crate::grants::{GrantLevel, GrantRecord};
    use crate::projects::Archived;

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
    fn holders(state: &crate::AppState, id: &str) -> Vec<String> {
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
            std::fs::canonicalize(fx.state.host.projects_root.clone().unwrap().join(&fx.name))
                .unwrap()
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
        assert!(vivi::registry::load().unwrap().get(&fx.root).is_none());
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
        assert!(vivi::registry::load().unwrap().get(&fx.root).is_some());
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
            vivi::registry::update(|registry| Ok(registry.remove(&fx.root)))
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
}
