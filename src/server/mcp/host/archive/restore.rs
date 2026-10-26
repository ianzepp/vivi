//! `restore_project`: undo an archive. The archived record names the root
//! and lanes directory exactly where archive left them, so restore moves
//! nothing: it re-registers the recorded root with the flags saved at
//! archive time, clears the record (which resets the purge clock), and
//! reconciles the host registry so the name is served again. No
//! `HOST_*` configuration is needed — the record carries everything — and
//! no operator key is minted again: the archive kept every grant.

use crate::registry::Flags;
use serde_json::{Value, json};

use crate::server::AppState;
use crate::server::hosted;

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
    crate::registry::update(|registry| {
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
#[path = "restore_test.rs"]
mod tests;
