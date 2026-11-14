//! The server side of the web console (`vivi-console`), mounted at `/ui`.
//!
//! The console never sees `AppState`. This host answers its one question:
//! which projects may the holder of this API key read? A server key
//! resolves through [`crate::server::grants::ServerKeyStore::verify`] to a
//! principal and its project grants. Each grant becomes a [`ProjectRef`]
//! when its project is hosted (it has a mailspace root), is not archived or
//! retired, and still has its `project.json`. Anything else is left out, so
//! the console shows exactly what the key may read and no more. A host
//! principal carries no grants, so it signs in to an empty project list.

use std::sync::Arc;

use axum::Router;
use vivi_console::{ConsoleHost, Principal, ProjectRef};

use crate::server::AppState;
use crate::server::grants::GrantRecord;
use crate::server::mcp::server::hosted_name;

/// Resolves console sign-in over the server's key store and project store.
pub struct ServerConsoleHost {
    state: AppState,
}

impl ServerConsoleHost {
    #[must_use]
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    /// The project a grant names, if the console may show it.
    fn project(&self, grant: &GrantRecord) -> Option<ProjectRef> {
        let Ok(meta) = self.state.projects.load_meta(&grant.project_id) else {
            return None;
        };
        if meta.is_archived() || self.state.projects.is_retired(&grant.project_id) {
            return None;
        }
        let root = meta.mailspace_root.clone()?;
        Some(ProjectRef {
            id: grant.project_id.clone(),
            name: hosted_name(&meta).unwrap_or_default(),
            mailspace_root: root,
        })
    }
}

impl ConsoleHost for ServerConsoleHost {
    fn authenticate(&self, key: &str) -> Option<Principal> {
        let verified = self.state.server_keys.verify(key)?;
        let mut projects: Vec<ProjectRef> = Vec::new();
        for grant in &verified.grants {
            let Some(project) = self.project(grant) else {
                continue;
            };
            if projects.iter().all(|seen| seen.id != project.id) {
                projects.push(project);
            }
        }
        Some(Principal {
            name: verified.principal,
            projects,
        })
    }
}

/// The console routes over `state`, ready to merge into the server router.
pub fn router(state: &AppState) -> Router {
    vivi_console::router(Arc::new(ServerConsoleHost::new(state.clone())))
}

#[cfg(test)]
#[path = "console_test.rs"]
mod tests;
