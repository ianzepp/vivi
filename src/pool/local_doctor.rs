//! The machine-host half of `vivi pool doctor` for the `local` host.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::doctor::Check;
use super::local::LocalProvider;
use super::local_git::Repo;
use super::local_proc;

pub const GROUP: &str = "Provider (local)";
/// Free space below which a warning is given, in GiB (same floor as
/// [`super::clean_rule::DISK_RECLAIM_GIB`]).
const LOW_DISK_GIB: u64 = super::clean_rule::DISK_RECLAIM_GIB;
const KIB_PER_GIB: u64 = 1024 * 1024;
/// Programs the scripts need on this machine.
const TOOLS: [&str; 6] = ["bash", "git", "tar", "ps", "df", "date"];

/// What the repository of a worktree pool looks like.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoFacts {
    pub worktrees: usize,
    pub default_branch: Option<String>,
    /// The branch checked out in the primary checkout.
    pub primary_branch: Option<String>,
}

/// What the probes saw.
#[derive(Debug, Clone)]
pub struct LocalFacts {
    pub root: String,
    pub system: String,
    pub missing: Vec<String>,
    /// Whether `ps` listed processes.
    pub processes: Result<usize, String>,
    pub root_exists: bool,
    pub root_writable: bool,
    /// The nearest directory of the root's path that exists.
    pub anchor: String,
    pub free_kb: Option<u64>,
    /// `Some` for a worktree pool: the path and what Git said about it.
    pub source: Option<(String, Result<RepoFacts, String>)>,
    pub harness_env: usize,
    pub unset_env: Vec<String>,
}

fn on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(name).is_file()))
}

/// The nearest ancestor of `root` that exists, and whether it is writable.
fn anchor_of(root: &Path) -> (PathBuf, bool) {
    let mut anchor = root.to_path_buf();
    while !anchor.exists() && anchor.pop() {}
    let writable = std::fs::metadata(&anchor).is_ok_and(|m| !m.permissions().readonly());
    (anchor, writable)
}

/// Free kilobytes where `path` lives, from `df -Pk`.
fn free_kb(path: &Path) -> Option<u64> {
    let out = Command::new("df")
        .arg("-Pk")
        .arg(path)
        .output()
        .map_or(None, Some)?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().nth(1)?;
    line.split_whitespace()
        .nth(3)?
        .parse::<u64>()
        .into_iter()
        .next()
}

fn repo_facts(path: &Path) -> Result<RepoFacts, String> {
    if !path.is_dir() {
        return Err(format!("{} is not a directory", path.display()));
    }
    let repo = Repo::new(path);
    repo.common_dir().map_err(|e| e.to_string())?;
    let trees = repo.worktrees().map_err(|e| e.to_string())?;
    Ok(RepoFacts {
        worktrees: trees.len(),
        default_branch: repo.default_branch(),
        primary_branch: trees.first().and_then(|t| t.branch.clone()),
    })
}

impl LocalProvider {
    /// The provider's checks, as the pool doctor lists them. Reads only: it
    /// creates nothing, not even the root.
    #[must_use]
    pub fn doctor_checks(&self, harness_env: &[String]) -> Vec<Check> {
        let (anchor, writable) = anchor_of(self.root());
        checks(&LocalFacts {
            root: self.root().display().to_string(),
            system: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
            missing: TOOLS
                .iter()
                .filter(|t| !on_path(t))
                .map(ToString::to_string)
                .collect(),
            processes: local_proc::listing()
                .map(|list| list.len())
                .map_err(|e| e.to_string()),
            root_exists: self.root().exists(),
            root_writable: writable,
            free_kb: free_kb(&anchor),
            anchor: anchor.display().to_string(),
            source: self
                .source()
                .map(|path| (path.display().to_string(), repo_facts(path))),
            harness_env: harness_env.len(),
            unset_env: harness_env
                .iter()
                .filter(|n| std::env::var_os(n).is_none_or(|v| v.is_empty()))
                .cloned()
                .collect(),
        })
    }
}

/// Turn what was seen into checks.
#[must_use]
pub fn checks(f: &LocalFacts) -> Vec<Check> {
    vec![
        system_check(f),
        tools_check(f),
        processes_check(f),
        root_check(f),
        disk_check(f),
        source_check(f),
        env_check(f),
    ]
}

fn system_check(f: &LocalFacts) -> Check {
    if matches!(std::env::consts::OS, "macos" | "linux") {
        Check::ok(
            GROUP,
            "system",
            format!(
                "{} (no isolation: the worker runs as you, with your whole account)",
                f.system
            ),
        )
    } else {
        Check::fail(
            GROUP,
            "system",
            format!("{} is not macOS or Linux", f.system),
            "the local provider runs on macOS or Linux",
        )
    }
}

fn tools_check(f: &LocalFacts) -> Check {
    if f.missing.is_empty() {
        return Check::ok(
            GROUP,
            "tools",
            "bash, git, tar, ps and the rest are on PATH",
        );
    }
    Check::fail(
        GROUP,
        "tools",
        format!("not on PATH: {}", f.missing.join(", ")),
        "install them or fix PATH for the shell that runs vivi (the local provider installs nothing)",
    )
}

fn processes_check(f: &LocalFacts) -> Check {
    match &f.processes {
        Ok(count) => Check::ok(
            GROUP,
            "process control",
            format!(
                "ps lists {count} processes (a slot is found by process id, start time, group and parentage)"
            ),
        ),
        Err(e) => Check::fail(
            GROUP,
            "process control",
            e.clone(),
            "install `ps` (procps) and put it on PATH: a slot's processes are found and ended with it",
        ),
    }
}

fn root_check(f: &LocalFacts) -> Check {
    match (f.root_exists, f.root_writable) {
        (true, true) => Check::ok(GROUP, "root", format!("{} exists and is writable", f.root)),
        (false, true) => Check::ok(
            GROUP,
            "root",
            format!("{} will be created by `vivi pool create`", f.root),
        ),
        _ => Check::fail(
            GROUP,
            "root",
            format!("{} ({}) is not writable", f.root, f.anchor),
            "choose a root you can write (`root` in [pools.<name>.local]), or fix its permissions",
        ),
    }
}

fn disk_check(f: &LocalFacts) -> Check {
    let Some(kb) = f.free_kb else {
        return Check::warn(GROUP, "disk", "free space could not be read", "check `df`");
    };
    let (gib, tenths) = (kb / KIB_PER_GIB, (kb % KIB_PER_GIB) * 10 / KIB_PER_GIB);
    let detail = format!(
        "{gib}.{tenths} GiB free where the root lives ({})",
        f.anchor
    );
    if gib < LOW_DISK_GIB {
        Check::warn(
            GROUP,
            "disk",
            detail,
            "each slot holds a checkout and its build output; free space or choose a root on a larger disk",
        )
    } else {
        Check::ok(GROUP, "disk", detail)
    }
}

fn source_check(f: &LocalFacts) -> Check {
    let Some((path, found)) = &f.source else {
        return Check::ok(
            GROUP,
            "source",
            "fetch mode: each slot fetches from a remote (no worktree_of set)",
        );
    };
    match found {
        Ok(facts) => Check::ok(
            GROUP,
            "source",
            format!(
                "worktree mode: {path} is a Git repository with {} worktree(s); primary branch {}, default branch {}",
                facts.worktrees,
                facts
                    .primary_branch
                    .as_deref()
                    .unwrap_or("none (detached or bare)"),
                facts.default_branch.as_deref().unwrap_or("unknown"),
            ),
        ),
        Err(e) => Check::fail(
            GROUP,
            "source",
            format!("worktree_of {path}: {e}"),
            "set `worktree_of` in [pools.<name>.local] to a Git repository on this machine",
        ),
    }
}

fn env_check(f: &LocalFacts) -> Check {
    let name = "harness env";
    if f.harness_env == 0 {
        return Check::ok(GROUP, name, "no harness env names configured");
    }
    if f.unset_env.is_empty() {
        return Check::ok(
            GROUP,
            name,
            format!(
                "all {} harness env names are set in this environment",
                f.harness_env
            ),
        );
    }
    Check::warn(
        GROUP,
        name,
        format!("not set in this environment: {}", f.unset_env.join(", ")),
        "export them in the shell that runs `vivi pool dispatch`; jobs read them from that process, and no value is stored",
    )
}

#[cfg(test)]
#[path = "local_doctor_test.rs"]
mod tests;
