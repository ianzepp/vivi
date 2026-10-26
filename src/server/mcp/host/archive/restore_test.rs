use crate::registry::{self, Flags};
use serde_json::json;

use super::super::super::HostConfig;
use super::super::test_support::{BASE_SECONDS, fixture};
use super::restore_project;
use crate::server::projects::Archived;

async fn tried(fx: &crate::server::AppState, name: &str) -> Result<serde_json::Value, String> {
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
