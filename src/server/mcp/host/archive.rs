//! Host archive lifecycle support: the fixed purge delay and the grant
//! cleanup every lifecycle step shares. The `restore` and `purge` child
//! modules hold the implementations (separate slices); no MCP tool is
//! exposed from here yet — the archive handlers land on this foundation.

/// How long an archived project waits between its archive stamp and the
/// moment `purge` may remove its files: 24 hours, fixed. There is no
/// environment override and no other production delay definition on
/// purpose: the delay is policy, not configuration.
pub(super) const PURGE_DELAY_SECONDS: i64 = 24 * 60 * 60;

pub(super) mod purge;
pub(super) mod restore;

#[cfg(test)]
mod test_support;

use super::super::tools::ToolError;
use crate::AppState;

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
    use crate::grants::{GrantLevel, GrantRecord};

    use super::super::create_project;
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

    /// The purge delay is one day, by policy: no override exists.
    #[test]
    fn purge_delay_is_one_day() {
        assert_eq!(super::PURGE_DELAY_SECONDS, 24 * 60 * 60);
    }
}
