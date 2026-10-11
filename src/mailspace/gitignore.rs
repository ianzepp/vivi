//! Keeping `.vivi/` out of git.
//!
//! Nothing under `.vivi/` is committed: the config (with the mailspace id),
//! charters, keys, store, blobs and logs are one person's machine-local
//! state, and must not propagate into a shared repository. The directory
//! ignores itself with a `.gitignore` whose `*` also matches that file, so a
//! repository sees nothing. The project's own root `.gitignore` is never
//! touched.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const IGNORE_FILE: &str = ".gitignore";

/// The exact content of `<.vivi>/.gitignore`.
pub const IGNORE_CONTENT: &str = "\
# Machine-local vivi state (config with the mailspace id, charters, keys, store,
# blobs, logs). Nothing in this directory is committed.
*
";

/// Write `<dir>/.gitignore` unless one exists; an existing file is never
/// edited, so user changes win. Returns whether this call created it. Any
/// failure (read-only or provisioned directories) is silent: the file is a
/// safeguard, never a requirement.
pub(super) fn ensure_ignore_file(dir: &Path) -> bool {
    let path = dir.join(IGNORE_FILE);
    let Ok(mut file) = OpenOptions::new().write(true).create_new(true).open(&path) else {
        return false;
    };
    file.write_all(IGNORE_CONTENT.as_bytes()).is_ok()
}

/// A warning when git tracks the mailspace config of the project at `root`,
/// else `None`. Best effort: a missing `git`, a directory that is not a
/// repository, or any failed call says nothing.
pub(super) fn tracked_config_warning(root: &Path, config_relpath: &str) -> Option<String> {
    let Ok(status) = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "--error-unmatch", "--", config_relpath])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    else {
        return None;
    };
    status.success().then(|| {
        format!(
            "warning: {config_relpath} is tracked by git, so every clone would share this \
             mailspace's id; run: git rm --cached -r .vivi"
        )
    })
}

#[cfg(test)]
#[path = "gitignore_test.rs"]
mod tests;
