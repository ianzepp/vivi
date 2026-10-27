//! `project_register` and `project_unregister`: put an existing mailspace
//! into the host registry so this server serves it, and take it out again.
//! Neither touches files on disk. A new entry is written already migrated
//! (`pump = true`); whether it pumps is the mailspace's own control state.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::mailspace::Mailspace;
use crate::registry::{self, Flags};
use crate::server::AppState;
use crate::server::hosted;

use super::super::tools::{ToolError, req_str};
use super::{CREATING, blocking, hosted_metas};

fn absolute_root(arguments: &Value) -> Result<PathBuf, ToolError> {
    let raw = req_str(arguments, "root")?;
    let path = Path::new(raw);
    if !path.is_absolute() {
        return Err(ToolError::new(format!(
            "root {raw:?} is not an absolute path"
        )));
    }
    Ok(path.to_path_buf())
}

/// Register an existing mailspace root and serve it. The `/v1/mcp` host tool.
pub(crate) async fn project_register(
    state: &AppState,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let given = absolute_root(arguments)?;
    let state = state.clone();
    blocking(move || {
        let _one = CREATING.lock().unwrap_or_else(|e| e.into_inner());
        register(&state, &given)
    })
    .await
}

/// Whether the project at `root` (or with `project_id`) is archived.
fn is_archived(state: &AppState, root: &Path, project_id: &str) -> bool {
    hosted_metas(state).into_iter().any(|meta| {
        meta.archived
            .as_ref()
            .is_some_and(|a| a.root == root || a.root.canonicalize().is_ok_and(|a| a == root))
            || (meta.project_id == project_id && meta.is_archived())
    })
}

/// Put `root` in the host registry with `serve` on.
fn mark_served(root: &Path) -> Result<(), ToolError> {
    registry::update(|registry| {
        // `pump = true` marks the entry as already migrated: the server
        // decides pumping from the mailspace's own control state, and a new
        // pump database starts stopped.
        let flags = registry.get(root).unwrap_or(Flags {
            pump: true,
            serve: true,
        });
        registry.set(
            root.to_path_buf(),
            Flags {
                serve: true,
                ..flags
            },
        );
        Ok(())
    })
    .map_err(|e| ToolError::new(format!("the host registry could not be updated: {e}")))
}

fn register(state: &AppState, given: &Path) -> Result<Value, ToolError> {
    if !given.join(".vivi").join("mailspace.toml").is_file() {
        return Err(ToolError::new(format!(
            "{} has no .vivi/mailspace.toml: it is not a mailspace root",
            given.display()
        )));
    }
    let root = given
        .canonicalize()
        .map_err(|e| ToolError::new(format!("cannot resolve {}: {e}", given.display())))?;
    crate::mailspace::inherit::refuse_base(&root).map_err(|e| ToolError::new(e.to_string()))?;
    let mailspace = Mailspace::open_root(root.clone())
        .map_err(|e| ToolError::new(format!("cannot open its mailspace: {e}")))?;
    let project_id = hosted::resolve_project_id(&mailspace).map_err(ToolError::new)?;
    if is_archived(state, &root, &project_id) {
        return Err(ToolError::new(format!(
            "{} is an archived project; restore_project brings it back",
            root.display()
        )));
    }
    mark_served(&root)?;
    let report = hosted::reconcile_registry(&state.projects, &state.identities);
    let (served, refusal) = report.map_or((false, None), |report| {
        (
            report.served.iter().any(|s| s.root == root),
            report
                .refused
                .iter()
                .find(|r| r.root == root)
                .map(|r| r.reason.clone()),
        )
    });
    let name = root
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .map_or_else(|| mailspace.config.name.clone(), str::to_owned);
    let mut out = json!({
        "root": root,
        "project_id": project_id,
        "name": name,
        "served": served,
    });
    if let Some(reason) = refusal {
        out["serve_error"] = json!(reason);
    }
    Ok(out)
}

/// Remove a root from the host registry. The `/v1/mcp` host tool.
pub(crate) async fn project_unregister(
    state: &AppState,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let given = absolute_root(arguments)?;
    let state = state.clone();
    blocking(move || {
        let _one = CREATING.lock().unwrap_or_else(|e| e.into_inner());
        let root = given.canonicalize().unwrap_or_else(|_| given.clone());
        let removed = registry::update(|registry| {
            Ok(registry.remove(&root) || (root != given && registry.remove(&given)))
        })
        .map_err(|e| ToolError::new(format!("the host registry could not be updated: {e}")))?;
        if !removed {
            return Err(ToolError::new(format!(
                "{} is not in the host registry",
                given.display()
            )));
        }
        hosted::reconcile_registry(&state.projects, &state.identities);
        Ok(json!({ "root": root, "unregistered": true }))
    })
    .await
}
