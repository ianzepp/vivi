//! What the console needs from the server that mounts it.
//!
//! The console never sees the server's state. The server answers one
//! question: which projects are there to show? There is no sign-in; the
//! console is meant for a trusted local network and only reads.

use std::path::PathBuf;

/// One project the console shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRef {
    /// The project id, as the server knows it.
    pub id: String,
    /// A friendly name; may be empty.
    pub name: String,
    /// The project root whose `.vivi/` holds the mailspace.
    pub mailspace_root: PathBuf,
}

/// The projects the console shows, as the server listed them for this
/// request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Catalog {
    pub projects: Vec<ProjectRef>,
}

impl Catalog {
    /// The project a URL names: an exact id first, else a unique name.
    #[must_use]
    pub fn project(&self, key: &str) -> Option<&ProjectRef> {
        if let Some(found) = self.projects.iter().find(|p| p.id == key) {
            return Some(found);
        }
        let mut named = self.projects.iter().filter(|p| p.name == key);
        match (named.next(), named.next()) {
            (Some(only), None) => Some(only),
            _ => None,
        }
    }

    /// The key a link uses for `project`: its name when that is unique among
    /// the projects shown, else its id.
    #[must_use]
    pub fn url_key<'a>(&self, project: &'a ProjectRef) -> &'a str {
        let shared = self
            .projects
            .iter()
            .filter(|other| other.name == project.name)
            .count();
        if project.name.is_empty() || shared > 1 {
            &project.id
        } else {
            &project.name
        }
    }
}

/// Implemented by the server. Sync on purpose: the console calls it from a
/// blocking task, so an implementation may read files.
pub trait ConsoleHost: Send + Sync {
    /// Every project the console shows, in display order.
    fn projects(&self) -> Vec<ProjectRef>;
}
