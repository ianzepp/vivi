//! Git as the `local` provider uses it against the repository a worktree pool
//! is made from.
//!
//! Only three commands change that repository: `git worktree add`, `git
//! worktree remove` and `git worktree prune`. Everything else here is a
//! read-only query. The three run under a lock (see [`Repo::lock`]) so that
//! two of them, in this process or in another `vivi`, never run at once.
//! Every command that changes the state of a checkout runs inside a slot's
//! own worktree, from the slot scripts, never here.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::VivariumError;

/// Environment that would point Git somewhere other than the repository
/// named on the command line (for example when `vivi` runs from a hook).
const GIT_ENV: [&str; 8] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_PREFIX",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
];

/// One worktree as `git worktree list --porcelain` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    /// The branch checked out, as a short name; none when detached or bare.
    pub branch: Option<String>,
    pub bare: bool,
}

/// A local Git repository, by the path the pool names.
#[derive(Debug, Clone)]
pub struct Repo {
    path: PathBuf,
}

/// Holds the repository's worktree lock until dropped (closing the file
/// releases the lock, also when the process dies).
#[derive(Debug)]
pub struct Held {
    _file: File,
}

fn git_error(what: &str, detail: &str) -> VivariumError {
    VivariumError::Message(format!("{what}: {}", detail.trim()))
}

/// Take the exclusive lock on `path` (a file or a directory; nothing is
/// created or written). Blocks until no other holder remains. The lock is
/// the operating system's `flock`: it works between threads and between
/// processes, and it goes when the holder does, also if it dies.
///
/// # Errors
/// Returns an error if the path cannot be opened or locked.
pub fn lock_path(path: &Path) -> Result<Held, VivariumError> {
    let file = File::open(path)
        .map_err(|e| git_error(&format!("cannot open {}", path.display()), &e.to_string()))?;
    file.lock()
        .map_err(|e| git_error(&format!("cannot lock {}", path.display()), &e.to_string()))?;
    Ok(Held { _file: file })
}

/// Take the lock on `path` only if it is free right now.
///
/// # Errors
/// Returns an error if the path cannot be opened.
pub fn try_lock_path(path: &Path) -> Result<Option<Held>, VivariumError> {
    let file = File::open(path)
        .map_err(|e| git_error(&format!("cannot open {}", path.display()), &e.to_string()))?;
    Ok(file.try_lock().is_ok().then_some(Held { _file: file }))
}

impl Repo {
    #[must_use]
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn git(&self) -> Command {
        let mut command = Command::new("git");
        for name in GIT_ENV {
            command.env_remove(name);
        }
        command.env("LC_ALL", "C").arg("-C").arg(&self.path);
        command
    }

    /// Run `git <args>`; its trimmed standard output, or the error with what
    /// Git said.
    fn run(&self, args: &[&str]) -> Result<String, VivariumError> {
        let out = self.git().args(args).output().map_err(|e| {
            VivariumError::Message(format!("failed to run `git` (is it on PATH?): {e}"))
        })?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            let said = String::from_utf8_lossy(&out.stderr);
            let what = format!("`git {}` in {}", args.join(" "), self.path.display());
            Err(git_error(&what, &said))
        }
    }

    /// Like [`Repo::run`], but a refusal is an absent answer.
    fn query(&self, args: &[&str]) -> Option<String> {
        self.run(args)
            .map_or(None, Some)
            .filter(|text| !text.is_empty())
    }

    /// The repository's common Git directory (the main `.git`, also when
    /// this path is a linked worktree).
    ///
    /// # Errors
    /// Returns an error if the path is not a Git repository.
    pub fn common_dir(&self) -> Result<PathBuf, VivariumError> {
        let raw = self.run(&["rev-parse", "--git-common-dir"])?;
        let dir = PathBuf::from(raw);
        let full = if dir.is_absolute() {
            dir
        } else {
            self.path.join(dir)
        };
        full.canonicalize()
            .map_err(|e| git_error("cannot resolve the Git directory", &e.to_string()))
    }

    /// Every worktree of the repository, the primary checkout first.
    ///
    /// # Errors
    /// Returns an error if Git cannot list them.
    pub fn worktrees(&self) -> Result<Vec<Worktree>, VivariumError> {
        let text = self.run(&["worktree", "list", "--porcelain"])?;
        Ok(parse_worktrees(&text))
    }

    /// The commit `rev` names, in full.
    #[must_use]
    pub fn commit_of(&self, rev: &str) -> Option<String> {
        self.query(&[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{commit}}"),
        ])
    }

    /// The tip of the local branch `name`, when it exists.
    #[must_use]
    pub fn branch_tip(&self, name: &str) -> Option<String> {
        self.commit_of(&format!("refs/heads/{name}"))
    }

    /// The repository's default branch: where `origin/HEAD` points, else the
    /// configured `init.defaultBranch`, else `main` or `master`, taking the
    /// first that exists as a local branch.
    #[must_use]
    pub fn default_branch(&self) -> Option<String> {
        let origin = self
            .query(&["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
            .and_then(|name| name.strip_prefix("origin/").map(str::to_string));
        let configured = self.query(&["config", "--get", "init.defaultBranch"]);
        origin
            .into_iter()
            .chain(configured)
            .chain(["main", "master"].map(str::to_string))
            .find(|name| self.branch_tip(name).is_some())
    }

    /// The lock that serializes `worktree add`, `remove` and `prune` on this
    /// repository, across processes: a `flock` on the repository's common
    /// Git directory itself, so nothing is added to the repository.
    ///
    /// # Errors
    /// Returns an error if the Git directory cannot be found or locked.
    pub fn lock(&self) -> Result<Held, VivariumError> {
        lock_path(&self.common_dir()?)
    }

    /// Make a detached worktree of the repository at `dest`, at `base`.
    ///
    /// # Errors
    /// Returns an error if Git refuses (for example `base` is not a commit).
    pub fn add_worktree(&self, dest: &Path, base: &str) -> Result<(), VivariumError> {
        let _held = self.lock()?;
        let dest = dest.to_string_lossy();
        self.run(&["worktree", "add", "--detach", &dest, base])
            .map(drop)
    }

    /// Remove the worktree at `dest` through Git, whatever it holds, and then
    /// prune the entries of worktrees whose directory is gone. Never touches
    /// a branch.
    ///
    /// # Errors
    /// Returns an error if Git refuses to remove it.
    pub fn remove_worktree(&self, dest: &Path) -> Result<(), VivariumError> {
        let _held = self.lock()?;
        if dest.exists() {
            let dest = dest.to_string_lossy();
            self.run(&["worktree", "remove", "--force", &dest])?;
        }
        self.run(&["worktree", "prune"]).map(drop)
    }

    /// Prune the entries of worktrees whose directory is gone.
    ///
    /// # Errors
    /// Returns an error if Git refuses.
    pub fn prune(&self) -> Result<(), VivariumError> {
        let _held = self.lock()?;
        self.run(&["worktree", "prune"]).map(drop)
    }
}

/// Read the blocks of `git worktree list --porcelain`.
#[must_use]
pub fn parse_worktrees(text: &str) -> Vec<Worktree> {
    let mut list: Vec<Worktree> = Vec::new();
    for line in text.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            list.push(Worktree {
                path: PathBuf::from(path),
                branch: None,
                bare: false,
            });
        } else if let Some(current) = list.last_mut() {
            if let Some(branch) = line.strip_prefix("branch ") {
                current.branch = Some(branch.trim_start_matches("refs/heads/").to_string());
            } else if line == "bare" {
                current.bare = true;
            }
        }
    }
    list
}

#[cfg(test)]
#[path = "local_git_test.rs"]
mod tests;
