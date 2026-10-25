//! `restore_project`: undo an archive. The archived record names the root
//! and lanes directory exactly where archive left them, so restore moves
//! nothing: it re-registers the recorded root with the flags saved at
//! archive time, clears the record (which resets the purge clock), and
//! reconciles the host registry so the name is served again. No
//! `HOST_*` configuration is needed — the record carries everything — and
//! no operator key is minted again: the archive kept every grant.

use serde_json::{Value, json};
use vivi::registry::Flags;

use crate::AppState;
use crate::hosted;

use super::super::super::tools::{ToolError, req_str};
use super::super::{CREATING, blocking, hosted_entry, same_dir};

/// Restore one archived project. The `/v1/mcp` host tool (wiring is its own
/// slice): `{"name": ...}`, no confirm. The answer is `{name, project_id,
/// root, lanes_dir, pump, serve, restored, served}` plus `serve_error` when
/// the restore asked for serving and reconciliation refused the root or
/// could not read the registry — the restore itself stands either way.
pub(crate) async fn restore_project(
    state: &AppState,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let name = req_str(arguments, "name")?.to_owned();
    let state = state.clone();
    blocking(move || {
        // One host-project mutation at a time, the same mutex every host
        // tool holds: the record read, the registry write and the record
        // clear are one step as far as the other tools are concerned.
        let _one = CREATING.lock().unwrap_or_else(|e| e.into_inner());
        restore(&state, &name)
    })
    .await
}

fn restore(state: &AppState, name: &str) -> Result<Value, ToolError> {
    let meta = hosted_entry(state, name)
        .ok_or_else(|| ToolError::new(format!("no hosted project named {name}")))?;
    let record = meta
        .archived
        .clone()
        .ok_or_else(|| ToolError::new(format!("{name} is not archived: nothing to restore")))?;
    if !record.root.is_dir() {
        return Err(ToolError::new(format!(
            "{name} is archived at {}, which no longer exists; the record is unchanged",
            record.root.display()
        )));
    }
    let root = record.root.clone();
    vivi::registry::update(|registry| {
        Ok(registry.set(
            root.clone(),
            Flags {
                pump: record.pump,
                serve: record.serve,
            },
        ))
    })
    .map_err(|e| ToolError::new(format!("could not register {}: {e}", root.display())))?;
    // The registry write succeeded: clearing the record now resets the
    // purge clock. Never cleared before a successful registry write.
    state
        .projects
        .set_archived(&meta.project_id, None)
        .map_err(|e| {
            ToolError::new(format!(
                "could not clear the archive record on {}: {e:?}",
                meta.project_id
            ))
        })?;
    let mut out = json!({
        "name": name,
        "project_id": meta.project_id,
        "root": root.display().to_string(),
        "lanes_dir": record.lanes_dir.display().to_string(),
        "pump": record.pump,
        "serve": record.serve,
        "restored": true,
    });
    let report = hosted::reconcile_registry(&state.projects, &state.identities);
    // Serving comes from the reconciliation report, never from the
    // requested flag alone.
    let served = report
        .as_ref()
        .is_some_and(|report| report.served.iter().any(|s| same_dir(&s.root, &root)));
    if record.serve && !served {
        let error = match report {
            None => "the host registry could not be read".to_owned(),
            Some(report) => report
                .refused
                .iter()
                .find(|r| same_dir(&r.root, &root))
                .map_or_else(
                    || "the project is not in the served set".to_owned(),
                    |r| r.reason.clone(),
                ),
        };
        out["serve_error"] = json!(error);
    }
    out["served"] = json!(served);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use vivi::registry::{self, Flags};

    use super::super::super::HostConfig;
    use super::super::test_support::{BASE_SECONDS, fixture};
    use super::restore_project;
    use crate::projects::Archived;

    async fn tried(fx: &crate::AppState, name: &str) -> Result<serde_json::Value, String> {
        restore_project(fx, &json!({ "name": name }))
            .await
            .map_err(|e| e.message)
    }

    /// A seeded archive with pump:true, serve:true restores under a blank
    /// `HOST_*` configuration: the record carries the root and the flags,
    /// so the project is served again, the record is gone, and the registry
    /// holds exactly the saved flags.
    #[tokio::test]
    async fn restores_the_saved_flags_without_host_configuration() {
        let mut fx = fixture("restore", true).await;
        fx.seed_archived(true, true);

        // Restore needs no HOST_* setting of any kind.
        fx.state.host = HostConfig::default();

        let out = tried(&fx.state, &fx.name).await.unwrap();

        assert_eq!(out["name"], json!(fx.name));
        assert_eq!(out["project_id"], json!(fx.project_id));
        assert_eq!(out["root"], json!(fx.root.display().to_string()));
        assert_eq!(out["lanes_dir"], json!(fx.lanes_dir.display().to_string()));
        assert_eq!(out["pump"], json!(true));
        assert_eq!(out["serve"], json!(true));
        assert_eq!(out["restored"], json!(true));
        assert_eq!(out["served"], json!(true));
        assert!(out.get("serve_error").is_none(), "{out}");

        let meta = fx
            .state
            .projects
            .load_meta_including_retired(&fx.project_id)
            .unwrap();
        assert!(meta.archived.is_none(), "{:?}", meta.archived);
        assert!(!fx.state.projects.is_retired(&fx.project_id));

        let registry = registry::load().unwrap();
        assert_eq!(
            registry.get(&fx.root),
            Some(Flags {
                pump: true,
                serve: true
            })
        );
    }

    /// A false/false record restores as false/false: the registry does not
    /// acquire `Flags::default()` (pump:true, serve:false), the project is
    /// not served, and no serve_error is reported because serving was not
    /// requested.
    #[tokio::test]
    async fn false_false_flags_stay_false_false() {
        let fx = fixture("restore-off", true).await;
        fx.seed_archived(false, false);

        let out = tried(&fx.state, &fx.name).await.unwrap();

        assert_eq!(out["pump"], json!(false));
        assert_eq!(out["serve"], json!(false));
        assert_eq!(out["restored"], json!(true));
        assert_eq!(out["served"], json!(false));
        assert!(out.get("serve_error").is_none(), "{out}");
        assert!(fx.state.projects.is_retired(&fx.project_id));

        let registry = registry::load().unwrap();
        assert_eq!(
            registry.get(&fx.root),
            Some(Flags {
                pump: false,
                serve: false
            })
        );
    }

    /// A project with no archive record is refused, and so is a name the
    /// server does not host at all.
    #[tokio::test]
    async fn refuses_a_project_that_is_not_archived() {
        let fx = fixture("restore-live", true).await;

        let error = tried(&fx.state, &fx.name).await.unwrap_err();
        assert_eq!(
            error,
            format!("{} is not archived: nothing to restore", fx.name)
        );

        let error = tried(&fx.state, "no-such-project").await.unwrap_err();
        assert_eq!(error, "no hosted project named no-such-project");

        // Neither touched the project's registration.
        assert_eq!(
            registry::load().unwrap().get(&fx.root),
            Some(Flags {
                pump: true,
                serve: true
            })
        );
    }

    /// A record whose root is gone is refused and the record stays exactly
    /// as seeded: nothing is registered and nothing is cleared.
    #[tokio::test]
    async fn refuses_and_keeps_the_record_when_the_root_is_gone() {
        let fx = fixture("restore-gone", true).await;
        fx.seed_archived(true, false);
        std::fs::remove_dir_all(&fx.root).unwrap();

        let error = tried(&fx.state, &fx.name).await.unwrap_err();
        assert!(
            error.contains("no longer exists"),
            "unexpected refusal: {error}"
        );

        let meta = fx
            .state
            .projects
            .load_meta_including_retired(&fx.project_id)
            .unwrap();
        assert_eq!(
            meta.archived,
            Some(Archived {
                at: fx.rfc3339(BASE_SECONDS),
                by: "mind".into(),
                root: fx.root.clone(),
                lanes_dir: fx.lanes_dir.clone(),
                pump: true,
                serve: false,
            })
        );
        assert!(registry::load().unwrap().get(&fx.root).is_none());
    }

    /// When serving fails after the registry registration — here the
    /// mailspace under the root is broken, so reconciliation refuses the
    /// root — the restore stands (record cleared, flags registered) and the
    /// answer reports `served: false` with a `serve_error`.
    #[tokio::test]
    async fn a_serving_failure_keeps_the_restore_and_reports_serve_error() {
        let fx = fixture("restore-serve", true).await;
        fx.seed_archived(true, true);
        std::fs::remove_dir_all(fx.root.join(".vivi")).unwrap();

        let out = tried(&fx.state, &fx.name).await.unwrap();

        assert_eq!(out["restored"], json!(true));
        assert_eq!(out["served"], json!(false));
        assert!(
            out["serve_error"]
                .as_str()
                .is_some_and(|e| e.contains("mailspace")),
            "{out}"
        );

        let meta = fx
            .state
            .projects
            .load_meta_including_retired(&fx.project_id)
            .unwrap();
        assert!(meta.archived.is_none(), "{:?}", meta.archived);
        let registry = registry::load().unwrap();
        assert_eq!(
            registry.get(&fx.root),
            Some(Flags {
                pump: true,
                serve: true
            })
        );
    }
}
