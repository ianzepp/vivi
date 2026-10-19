//! Git as the `local` provider uses it against the repository a worktree pool
//! is made from.
//!
//! Commands that change that repository: `git worktree add`, `git
//! worktree remove`, scoped removal of a vanished Vivi worktree entry,
//! `git branch -d` of a merged scratch
//! branch, and the single-branch `git fetch` of [`Repo::fetch_remote_branch`].
//! Everything else here is a read-only query. The three run under a lock (see [`Repo::lock`]) so that
//! two of them, in this process or in another `vivi`, never run at once.
//! Every command that changes the state of a checkout runs inside a slot's
//! own worktree, from the slot scripts, never here.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::error::VivariumError;

/// Canonicalize a path, including a missing tail, through its nearest
/// existing ancestor.
#[must_use]
pub fn real_path(path: &Path) -> PathBuf {
    if let Ok(resolved) = path.canonicalize() {
        return resolved;
    }
    for ancestor in path.ancestors().skip(1) {
        if let Ok(resolved) = ancestor.canonicalize() {
            if let Ok(tail) = path.strip_prefix(ancestor) {
                return resolved.join(tail);
            }
        }
    }
    path.to_path_buf()
}

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

/// The longest a single `git fetch` of one remote branch may run.
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

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

    /// Configured fetch source for a named remote.
    #[must_use]
    pub fn remote_url(&self, remote: &str) -> Option<String> {
        self.query(&["remote", "get-url", remote])
    }

    /// The names of the remotes configured in the repository.
    #[must_use]
    pub fn remotes(&self) -> Vec<String> {
        self.query(&["remote"])
            .map(|text| text.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }

    /// The `(remote, branch)` that `rev` names when it has the shape
    /// `<remote>/<branch>` and `<remote>` is configured here. A plain branch
    /// name, a sha and any revision expression (`~`, `^`, `:` ...) name none.
    #[must_use]
    pub fn remote_branch_of(&self, rev: &str) -> Option<(String, String)> {
        if rev.starts_with('-') || rev.contains(|c: char| " :*?[\\^~@".contains(c)) {
            return None;
        }
        let mut remotes = self.remotes();
        remotes.sort_by_key(|name| std::cmp::Reverse(name.len()));
        remotes.into_iter().find_map(|remote| {
            let branch = rev.strip_prefix(&format!("{remote}/"))?;
            (!branch.is_empty() && !branch.ends_with('/') && !branch.contains(".."))
                .then(|| (remote, branch.to_string()))
        })
    }

    /// Fetch the one branch `branch` of `remote` into its remote-tracking ref
    /// `refs/remotes/<remote>/<branch>`, bounded by [`FETCH_TIMEOUT`].
    ///
    /// # Errors
    /// Returns what Git said (its last lines), or that the fetch timed out.
    pub fn fetch_remote_branch(&self, remote: &str, branch: &str) -> Result<(), VivariumError> {
        let spec = format!("+refs/heads/{branch}:refs/remotes/{remote}/{branch}");
        let mut child = self
            .git()
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(["fetch", "-q", remote, &spec])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| git_error("failed to run `git fetch`", &e.to_string()))?;
        let what = format!("`git fetch {remote} {branch}`");
        let deadline = Instant::now() + FETCH_TIMEOUT;
        loop {
            let waited = child.try_wait();
            if let Ok(Some(status)) = waited {
                let out = child
                    .wait_with_output()
                    .map_err(|e| git_error("failed to read `git fetch` output", &e.to_string()))?;
                return if status.success() {
                    Ok(())
                } else {
                    Err(git_error(
                        &format!("{what} failed"),
                        &tail(&String::from_utf8_lossy(&out.stderr)),
                    ))
                };
            }
            if waited.is_err() || Instant::now() >= deadline {
                drop(child.kill());
                drop(child.wait());
                return Err(git_error(&what, "timed out"));
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// The commit `rev` names. When it names none locally and has the shape
    /// `<remote>/<branch>` of a configured remote, fetch that one branch once
    /// and look again. The second field is the fetch error, when a fetch was
    /// tried and the revision still names no commit.
    #[must_use]
    pub fn commit_of_fetching(&self, rev: &str) -> (Option<String>, Option<String>) {
        if let Some(commit) = self.commit_of(rev) {
            return (Some(commit), None);
        }
        let Some((remote, branch)) = self.remote_branch_of(rev) else {
            return (None, None);
        };
        let fetched = self.fetch_remote_branch(&remote, &branch);
        match (self.commit_of(rev), fetched) {
            (Some(commit), _) => (Some(commit), None),
            (None, Err(e)) => (None, Some(e.to_string())),
            (None, Ok(())) => (None, None),
        }
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

    /// The lock that serializes `worktree add` and `remove` on this
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

    /// Point this checkout at `rev` on a detached HEAD.
    ///
    /// # Errors
    /// Returns an error if Git refuses the checkout.
    pub fn checkout_detach(&self, rev: &str) -> Result<(), VivariumError> {
        self.run(&["checkout", "-q", "--detach", rev]).map(drop)
    }

    /// Remove the exact worktree at `dest` through Git. Never prunes other
    /// entries or touches a branch.
    ///
    /// # Errors
    /// Returns an error if Git refuses to remove it.
    pub fn remove_worktree(&self, dest: &Path) -> Result<(), VivariumError> {
        let _held = self.lock()?;
        if dest.exists() {
            let dest = dest.to_string_lossy();
            self.run(&["worktree", "remove", "--force", &dest])?;
        }
        Ok(())
    }

    /// Remove a worktree only if Git considers it clean enough to drop
    /// without `--force`.
    ///
    /// # Errors
    /// Returns an error if Git refuses (dirty tree, locked, ...).
    pub fn remove_worktree_clean(&self, dest: &Path) -> Result<(), VivariumError> {
        let _held = self.lock()?;
        if dest.exists() {
            let dest = dest.to_string_lossy();
            self.run(&["worktree", "remove", &dest])?;
        }
        Ok(())
    }

    /// Remove only the Git admin entry for a vanished worktree under
    /// `pool_root`. Other prunable entries in the source repository are left
    /// in place.
    ///
    /// # Errors
    /// Returns an error if the path is outside the pool, Git cannot inspect
    /// the repository, or its exact admin entry cannot safely be removed.
    pub fn remove_gone_worktree_under(
        &self,
        dest: &Path,
        pool_root: &Path,
    ) -> Result<(), VivariumError> {
        let _held = self.lock()?;
        let wanted = gone_worktree_path(dest, pool_root)?;
        if !self
            .worktrees()?
            .iter()
            .any(|tree| !tree.bare && real_path(&tree.path) == wanted)
        {
            return Ok(());
        }
        remove_worktree_admin_entry(&self.common_dir()?.join("worktrees"), &wanted)
    }

    /// Paths `git status --porcelain` names in this checkout.
    ///
    /// # Errors
    /// Returns an error if Git cannot run.
    pub fn dirty_paths(&self) -> Result<Vec<String>, VivariumError> {
        let text = self.run(&["status", "--porcelain"])?;
        Ok(text
            .lines()
            .filter_map(|line| {
                let rest = line.get(2..)?;
                let path = rest.strip_prefix(' ').unwrap_or(rest).trim();
                (!path.is_empty()).then(|| path.replace(" -> ", " "))
            })
            .collect())
    }

    /// True when `commit` is an ancestor of `of` (or they are the same).
    #[must_use]
    pub fn is_ancestor(&self, commit: &str, of: &str) -> bool {
        self.run(&["merge-base", "--is-ancestor", commit, of])
            .is_ok()
    }

    /// Commits reachable from `head` and not from `base`, newest first, at
    /// most 20.
    ///
    /// # Errors
    /// Returns an error if Git cannot run.
    pub fn commits_ahead(&self, head: &str, base: &str) -> Result<Vec<String>, VivariumError> {
        let spec = format!("{base}..{head}");
        let text = self.run(&["rev-list", "--max-count=20", &spec])?;
        Ok(text
            .lines()
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// Delete a local branch that Git considers merged (`git branch -d`).
    ///
    /// # Errors
    /// Returns an error if the branch is not merged or Git refuses.
    pub fn delete_merged_branch(&self, name: &str) -> Result<(), VivariumError> {
        let _held = self.lock()?;
        self.run(&["branch", "-d", name]).map(drop)
    }
}

fn gone_worktree_path(dest: &Path, pool_root: &Path) -> Result<PathBuf, VivariumError> {
    if dest.exists() {
        return Err(VivariumError::Message(format!(
            "refusing to remove a live worktree entry: {}",
            dest.display()
        )));
    }
    let root = real_path(pool_root);
    let wanted = real_path(dest);
    if wanted == root || !wanted.starts_with(&root) {
        return Err(VivariumError::Message(format!(
            "refusing to remove worktree outside pool directory {}: {}",
            root.display(),
            wanted.display()
        )));
    }
    Ok(wanted)
}

fn remove_worktree_admin_entry(admin_root: &Path, wanted: &Path) -> Result<(), VivariumError> {
    let entries = match std::fs::read_dir(admin_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        let admin = entry.path();
        let file_type = entry.file_type()?;
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        let gitdir = match std::fs::read_to_string(admin.join("gitdir")) {
            Ok(path) => PathBuf::from(path.trim()),
            Err(_) => continue,
        };
        let Some(worktree) = gitdir.parent() else {
            continue;
        };
        if real_path(worktree) != wanted {
            continue;
        }
        if admin.join("locked").exists() {
            return Err(VivariumError::Message(format!(
                "refusing to remove locked worktree entry for {}",
                wanted.display()
            )));
        }
        std::fs::remove_dir_all(&admin)?;
        return Ok(());
    }
    Err(VivariumError::Message(format!(
        "cannot find the exact Git worktree entry for {}",
        wanted.display()
    )))
}

/// The last few lines of what Git said, on one line.
fn tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(3)..].join("; ")
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
