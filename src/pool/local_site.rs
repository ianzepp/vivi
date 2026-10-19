//! Resolution of a configured local pool root into the on-disk pool
//! directory for one mailspace (see [`LocalSite`]).

use std::path::{Path, PathBuf};

use crate::error::VivariumError;

/// A resolved local pool site: the pool root plus the directory that holds
/// this mailspace's slots inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSite {
    /// Pool root: the expanded `~/` root, the `/`-absolute root as given, or
    /// the project-relative directory.
    pub root: PathBuf,
    /// Directory holding this mailspace's slots inside the pool root.
    pub pool_dir: PathBuf,
    /// `true` when the root is project-relative.
    pub relative: bool,
    /// Mailspace name, as given.
    pub mailspace: String,
    /// Pool name, as given.
    pub pool: String,
}

/// Resolve a configured local pool root into the pool directory for one
/// mailspace.
///
/// A root starting `~/` expands to the home directory; a root starting `/`
/// is used as-is; in both cases the mailspace segment is kept
/// (`<root>/<mailspace>/<pool>`). Any other root is project-relative
/// (`<root>/<pool>`) and must name a plain path inside the project.
pub fn resolve(
    root: &str,
    project: &Path,
    mailspace: &str,
    pool: &str,
) -> Result<LocalSite, VivariumError> {
    let (root_path, relative) = if root.starts_with("~/") {
        (super::local_slots::expand_root(root), false)
    } else if root.starts_with('/') {
        (PathBuf::from(root), false)
    } else {
        let path = project.join(root);
        refuse_unsafe_relative(root, project, &path)?;
        (path, true)
    };
    let pool_dir = if relative {
        root_path.join(pool)
    } else {
        root_path.join(mailspace).join(pool)
    };
    Ok(LocalSite {
        root: root_path,
        pool_dir,
        relative,
        mailspace: mailspace.to_string(),
        pool: pool.to_string(),
    })
}

/// A project-relative root must stay inside the project and use plain parts.
fn refuse_unsafe_relative(root: &str, project: &Path, resolved: &Path) -> Result<(), VivariumError> {
    if let Some(part) = root
        .split('/')
        .find(|part| part.is_empty() || *part == "." || *part == "..")
    {
        return Err(VivariumError::Config(format!(
            "local.root {root:?} must use plain path parts only: {part:?} is empty, \".\" or \"..\""
        )));
    }
    if root == ".vivi" || resolved == project.join(".vivi") || resolved == project {
        return Err(VivariumError::Config(
            "local.root must stay inside the project and must not be the project root or .vivi itself"
                .to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "local_site_test.rs"]
mod tests;
