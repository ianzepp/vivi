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

use crate::registry::Flags;
use chrono::SecondsFormat;
use serde_json::{Value, json};

use super::super::tools::{ToolError, map_project, req_str};
use crate::server::AppState;
use crate::server::projects::Archived;

/// Archive one hosted project. A state-only retirement: stamp the archive
/// record onto the project's metadata, take its mailspace root out of the
/// host registry, and leave every file exactly where it lies — a pump may
/// still hold `.vivi/pump/lock` mid-tick, and archive succeeds anyway.
///
/// Requires `name`, a `confirm` that repeats it and a name that passes
/// [`crate::project_create::check_name`] before anything is touched, then
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
    crate::project_create::check_name(&name).map_err(|e| ToolError::new(e.to_string()))?;
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
                crate::registry::update(|registry| Ok(registry.remove(&existing.root))).map_err(
                    |e| ToolError::new(format!("the host registry could not be updated: {e}")),
                )?;
            crate::server::hosted::reconcile_registry(&state.projects, &state.identities);
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
        let registry = crate::registry::load()
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
        let unregistered = crate::registry::update(|registry| Ok(registry.remove(&root)))
            .map_err(|e| ToolError::new(format!("the host registry could not be updated: {e}")))?;
        crate::server::hosted::reconcile_registry(&state.projects, &state.identities);
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
///
/// A final-grant holder has its keys revoked before the grant comes off.
/// The two are separate saves, and a retry finds its principals by the grant
/// they still hold: revoking first means an interruption between the writes
/// leaves an inert grant (its keys no longer verify) that the next run still
/// finds and finishes, never a removed grant with live keys that no run
/// would revoke.
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
        state
            .server_keys
            .remove_grant(&principal.principal, project_id)
            .map_err(|e| {
                ToolError::new(format!(
                    "could not remove the grant on {project_id} from {}: {e:?}",
                    principal.principal
                ))
            })?;
        ungranted.push(principal.principal);
    }
    Ok((ungranted, revoked))
}

#[cfg(test)]
#[path = "archive_test.rs"]
mod tests;
