//! What the console needs from the server that mounts it.
//!
//! The console never sees the server's state. The server answers one
//! question: who does this key belong to, and which projects may that
//! principal read?

use std::path::PathBuf;

/// One project a principal may read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRef {
    /// The project id, as the server knows it.
    pub id: String,
    /// A friendly name; may be empty.
    pub name: String,
    /// The project root whose `.vivi/` holds the mailspace.
    pub mailspace_root: PathBuf,
}

/// Who a key belongs to and what that principal may read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub name: String,
    pub projects: Vec<ProjectRef>,
}

impl Principal {
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
    /// this principal's projects, else its id.
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
    /// Resolve a presented API key. `None` for an unknown or revoked key.
    /// The key must never be logged or kept.
    fn authenticate(&self, key: &str) -> Option<Principal>;
}
