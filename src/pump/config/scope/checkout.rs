//! Filesystem inspection for checked scope authorization: what a requested
//! scope path names inside a target's integration checkout.

use std::io::ErrorKind;
use std::path::Path;

use super::{ScopePath, ScopePathKind};

/// What the checkout says a requested scope path names, as far as
/// conservative inspection can tell without following symlinks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OnDisk {
    /// The leaf does not exist; every existing component on the way is a
    /// verified real directory inside the checkout.
    New,
    /// The leaf exists and is not a directory.
    File,
    /// The leaf exists as a real directory.
    Directory,
}

/// Classify one parsed request path against the checkout. The checkout root
/// itself must be a real, accessible directory: a missing, symlinked or
/// unreadable root makes every request unverifiable. `None` is the verdict
/// an operator must look at: an existing symlink anywhere on the way
/// (including a requested final symlink), a dangling alias, an inaccessible
/// component, or any other traversal the filesystem will not answer for. A
/// missing component ends the walk as [`OnDisk::New`]: nothing can hide
/// beneath a missing directory, and new leaves beneath a verifiable
/// checkout are exactly what hand work creates.
pub(super) fn classify(checkout: &Path, path: &ScopePath) -> Option<OnDisk> {
    let root = match std::fs::symlink_metadata(checkout) {
        Ok(root) => root,
        Err(_) => return None,
    };
    if root.is_symlink() || !root.is_dir() {
        return None;
    }
    if path.kind() == ScopePathKind::Root {
        return Some(OnDisk::Directory);
    }
    let parts: Vec<&str> = path.as_str().trim_end_matches('/').split('/').collect();
    let mut walked = checkout.to_path_buf();
    for (index, part) in parts.iter().enumerate() {
        walked.push(part);
        let meta = match std::fs::symlink_metadata(&walked) {
            Ok(meta) => meta,
            Err(error) if error.kind() == ErrorKind::NotFound => return Some(OnDisk::New),
            Err(_) => return None,
        };
        if meta.is_symlink() {
            return None;
        }
        if meta.is_dir() {
            continue;
        }
        // An existing non-directory is the requested leaf or it is
        // uncertain traversal; nothing can be verified through it.
        if index + 1 == parts.len() {
            return Some(OnDisk::File);
        }
        return None;
    }
    Some(OnDisk::Directory)
}

#[cfg(test)]
#[path = "checkout_test.rs"]
mod tests;
