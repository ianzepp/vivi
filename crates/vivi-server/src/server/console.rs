//! The server side of the web console (`vivi-console`), mounted at `/ui`.
//!
//! The console never sees `AppState`. This host answers its one question:
//! which projects are there to show? Every hosted project the server serves,
//! that is: it has a `project.json` with a mailspace root, and it is neither
//! archived nor retired. There is no sign-in; the console is for a trusted
//! local network and only reads.

use std::sync::Arc;

use axum::Router;
use vivi_console::{ConsoleHost, ProjectRef};

use crate::server::AppState;
use crate::server::hosted::hosted_ids_on_disk;
use crate::server::mcp::server::hosted_name;

/// Lists the server's hosted projects for the console.
pub struct ServerConsoleHost {
    state: AppState,
}

impl ServerConsoleHost {
    #[must_use]
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    /// The project `id` names, if the console may show it.
    fn project(&self, id: &str) -> Option<ProjectRef> {
        let Ok(meta) = self.state.projects.load_meta(id) else {
            return None;
        };
        if meta.is_archived() || self.state.projects.is_retired(id) {
            return None;
        }
        let root = meta.mailspace_root.clone()?;
        Some(ProjectRef {
            id: id.to_string(),
            name: hosted_name(&meta).unwrap_or_default(),
            mailspace_root: root,
        })
    }
}

impl ConsoleHost for ServerConsoleHost {
    fn projects(&self) -> Vec<ProjectRef> {
        let mut projects: Vec<ProjectRef> = hosted_ids_on_disk(&self.state.projects)
            .iter()
            .filter_map(|id| self.project(id))
            .collect();
        projects.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        projects
    }
}

/// The console routes over `state`, ready to merge into the server router.
pub fn router(state: &AppState) -> Router {
    vivi_console::router(Arc::new(ServerConsoleHost::new(state.clone())))
}

#[cfg(test)]
#[path = "console_test.rs"]
mod tests;
