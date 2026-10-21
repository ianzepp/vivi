//! A pool source that is a plain Git remote instead of a GitHub repository.
//!
//! `[pools.<name>] remote_url` names an SSH (`user@host:path`, `ssh://`) or
//! `file://` address. There is no App token and no GitHub API: refs resolve
//! with `git ls-remote` on this machine, and the slots fetch and push the
//! address with the login user's own SSH key (the runner passes any address
//! that is not `https` through unchanged).

use std::process::Command;

use super::config::{PoolConfig, RepoAccess};
use crate::VivariumError;

/// Where a pool's jobs fetch from and push to.
#[derive(Debug, Clone, Copy)]
pub enum RepoSource<'a> {
    /// `repo = "owner/name"` with its GitHub App.
    Github(RepoAccess<'a>),
    /// `remote_url`: a plain Git address.
    Git(&'a str),
}

impl PoolConfig {
    /// The pool's source for a command that needs one (`what` says which).
    ///
    /// # Errors
    /// Returns a configuration error when neither or both of `repo` and
    /// `remote_url` are set, when `remote_url` is not a plain Git address,
    /// or when a GitHub source lacks its App.
    pub fn require_source(&self, pool: &str, what: &str) -> Result<RepoSource<'_>, VivariumError> {
        match (&self.repo, &self.remote_url) {
            (Some(_), Some(_)) => Err(VivariumError::Config(format!(
                "[pools.{pool}] sets both repo (GitHub) and remote_url (plain Git); keep one"
            ))),
            (None, Some(url)) => {
                check_address(pool, url)?;
                Ok(RepoSource::Git(url))
            }
            _ => self.require_repo(pool, what).map(RepoSource::Github),
        }
    }

    /// True when the pool names a source of either kind.
    #[must_use]
    pub fn has_source(&self) -> bool {
        self.repo.is_some() || self.remote_url.is_some()
    }
}

/// A `remote_url` must be an address Git reaches without a token.
fn check_address(pool: &str, url: &str) -> Result<(), VivariumError> {
    if url.starts_with("https://") || url.starts_with("http://") {
        return Err(VivariumError::Config(format!(
            "[pools.{pool}] remote_url {url} is an http(s) address; use repo and [pools.{pool}.github] for GitHub, or an SSH or file:// address for a plain Git remote"
        )));
    }
    if url.is_empty() || url.starts_with('-') || url.chars().any(char::is_whitespace) {
        return Err(VivariumError::Config(format!(
            "[pools.{pool}] remote_url {url:?} is not a Git address"
        )));
    }
    Ok(())
}

/// The remote's default branch, from the `HEAD` symref it advertises.
///
/// # Errors
/// Returns an error when the remote is unreachable or advertises no `HEAD`.
pub fn default_branch(remote: &str) -> Result<String, VivariumError> {
    let out = ls_remote(remote, &["--symref"], &["HEAD"])?;
    parse_symref(&out)
        .ok_or_else(|| VivariumError::Message(format!("{remote} advertises no default branch")))
}

/// Resolve `requested` (a branch, a tag, or a full commit hash) to a commit;
/// `None` means the default branch. A full hash is taken as given: the slot's
/// fetch is what proves it exists. An abbreviated hash cannot be resolved
/// without a fetch, so it is refused.
///
/// # Errors
/// Returns an error for a malformed or unknown ref, an abbreviated hash, or
/// an unreachable remote.
pub fn resolve_base(remote: &str, requested: Option<&str>) -> Result<String, VivariumError> {
    let name = match requested {
        Some(r) => r.to_string(),
        None => default_branch(remote)?,
    };
    check_ref(&name)?;
    if is_full_hash(&name) {
        return Ok(name.to_ascii_lowercase());
    }
    if is_hex(&name) && (4..40).contains(&name.len()) {
        return Err(VivariumError::Message(format!(
            "\"{name}\" is an abbreviated commit; a plain Git remote needs the full hash or a branch or tag name"
        )));
    }
    let heads = format!("refs/heads/{name}");
    let tags = format!("refs/tags/{name}");
    let peeled = format!("refs/tags/{name}^{{}}");
    // The peeled line of an annotated tag is listed only when a pattern
    // names it; without it a tag would resolve to the tag object.
    let out = ls_remote(remote, &[], &[&heads, &tags, &peeled])?;
    pick_commit(&out, &[&heads, &peeled, &tags])
        .ok_or_else(|| VivariumError::Message(format!("{remote} has no branch or tag \"{name}\"")))
}

/// The `ref: refs/heads/<branch>\tHEAD` line of `ls-remote --symref`.
#[must_use]
pub fn parse_symref(out: &str) -> Option<String> {
    out.lines().find_map(|line| {
        let (target, name) = line.strip_prefix("ref: ")?.split_once('\t')?;
        (name == "HEAD").then_some(target.strip_prefix("refs/heads/")?.to_string())
    })
}

/// The commit of the first of `wanted` that `ls-remote` listed.
#[must_use]
pub fn pick_commit(out: &str, wanted: &[&str]) -> Option<String> {
    let listed: Vec<(&str, &str)> = out
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .collect();
    wanted.iter().find_map(|want| {
        listed
            .iter()
            .find(|(_, name)| name == want)
            .map(|(oid, _)| (*oid).to_string())
    })
}

fn check_ref(reference: &str) -> Result<(), VivariumError> {
    let ok = !reference.is_empty()
        && !reference.starts_with('-')
        && reference
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'));
    if ok {
        Ok(())
    } else {
        Err(VivariumError::Message(format!(
            "base \"{reference}\" is not a plain commit, branch, or tag name"
        )))
    }
}

fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn is_full_hash(s: &str) -> bool {
    is_hex(s) && (s.len() == 40 || s.len() == 64)
}

/// `git ls-remote` that never prompts: no terminal credential prompt, and an
/// SSH that fails fast instead of asking for a passphrase or a host key,
/// unless the caller set `GIT_SSH_COMMAND` themselves.
fn ls_remote(remote: &str, options: &[&str], patterns: &[&str]) -> Result<String, VivariumError> {
    let mut cmd = Command::new("git");
    cmd.arg("ls-remote")
        .args(options)
        .arg(remote)
        .args(patterns)
        .env("GIT_TERMINAL_PROMPT", "0");
    if std::env::var_os("GIT_SSH_COMMAND").is_none() {
        cmd.env(
            "GIT_SSH_COMMAND",
            "ssh -o BatchMode=yes -o ConnectTimeout=15",
        );
    }
    let out = cmd.output()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(VivariumError::Message(format!(
            "git ls-remote {remote} failed: {}",
            err.trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
#[path = "plain_git_test.rs"]
mod tests;
