//! The repository-host half of `vivi pool doctor`: what GitHub and the local
//! signing tool say about the pool's repository.

use std::collections::BTreeMap;
use std::process::Command;

use super::config::{PoolGithub, RepoAccess};
use super::doctor::Check;
use super::github;

pub const GROUP: &str = "Repository (GitHub)";

/// What the gather step saw. `None` means the step was not attempted because
/// an earlier one failed.
#[derive(Debug, Clone, Default)]
pub struct GithubView {
    /// True when the pool has no repository configured: nothing was looked at.
    pub absent: bool,
    /// `openssl` version line.
    pub openssl: Option<Result<String, String>>,
    /// The key path, when the file is readable.
    pub key: Option<Result<String, String>>,
    /// A token minted for the repo (the token itself is dropped).
    pub token: Option<Result<(), String>>,
    pub branch: Option<Result<String, String>>,
    /// Whether GitHub reports the default branch as protected.
    pub protection: Option<Result<bool, String>>,
    /// Commits the default branch is ahead of each seeded base, by base.
    pub behind: BTreeMap<String, Result<u64, String>>,
}

/// Look at GitHub for real. Every step after the token needs the token, so a
/// failed token ends the walk. Nothing is written anywhere.
#[must_use]
pub fn gather(access: &RepoAccess, bases: &[String]) -> GithubView {
    let mut view = GithubView {
        openssl: Some(openssl_version()),
        key: Some(key_readable(access.github)),
        ..GithubView::default()
    };
    let signs = matches!(view.openssl, Some(Ok(_))) && matches!(view.key, Some(Ok(_)));
    if !signs {
        return view;
    }
    let token = match github::mint_token(access) {
        Ok(token) => token,
        Err(e) => {
            view.token = Some(Err(e.to_string()));
            return view;
        }
    };
    view.token = Some(Ok(()));
    let branch = github::default_branch(&token, access.repo).map_err(|e| e.to_string());
    if let Ok(name) = &branch {
        view.protection =
            Some(github::branch_protected(&token, access.repo, name).map_err(|e| e.to_string()));
        for base in bases {
            let ahead = github::commits_ahead(&token, access.repo, base, name);
            view.behind
                .insert(base.clone(), ahead.map_err(|e| e.to_string()));
        }
    }
    view.branch = Some(branch);
    view
}

fn openssl_version() -> Result<String, String> {
    let output = Command::new("openssl")
        .arg("version")
        .output()
        .map_err(|e| format!("cannot run openssl: {e}"))?;
    if !output.status.success() {
        return Err("openssl did not run cleanly".to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn key_readable(github: &PoolGithub) -> Result<String, String> {
    let path = github.private_key_path();
    std::fs::File::open(&path)
        .map(|_| path.display().to_string())
        .map_err(|e| format!("cannot read {}: {e}", path.display()))
}

/// The one line for a pool with no repository configured.
#[must_use]
pub fn absent_check() -> Check {
    Check::skipped(
        GROUP,
        "repository",
        "no repository configured for this pool",
    )
}

/// Turn what was seen into checks.
#[must_use]
pub fn checks(repo: &str, view: &GithubView) -> Vec<Check> {
    let branch_name = match &view.branch {
        Some(Ok(name)) => Some(name.as_str()),
        _ => None,
    };
    vec![
        step(
            "openssl",
            view.openssl.as_ref(),
            "install openssl (it signs the App token)",
            "",
        ),
        step(
            "app key",
            view.key.as_ref(),
            "set [github] private_key to a readable PEM file",
            "",
        ),
        token_check(repo, view),
        step(
            "default branch",
            view.branch.as_ref(),
            &format!("check that {repo} exists and the App is installed on it"),
            "needs a token",
        ),
        protection_check(branch_name, view),
    ]
}

/// One check from an optional result: ok with the text, fail with `fix`, or
/// skipped with `reason` when the step was not attempted.
fn step(name: &str, result: Option<&Result<String, String>>, fix: &str, reason: &str) -> Check {
    match result {
        Some(Ok(text)) => Check::ok(GROUP, name, text.clone()),
        Some(Err(e)) => Check::fail(GROUP, name, e.clone(), fix),
        None => Check::skipped(GROUP, name, skip_reason(name, reason)),
    }
}

fn skip_reason(name: &str, reason: &str) -> String {
    if reason.is_empty() {
        format!("{name} did not run")
    } else {
        reason.to_string()
    }
}

fn token_check(repo: &str, view: &GithubView) -> Check {
    match &view.token {
        Some(Ok(())) => Check::ok(
            GROUP,
            "token",
            format!("minted for {repo} (one hour; not shown)"),
        ),
        Some(Err(e)) => Check::fail(
            GROUP,
            "token",
            e.clone(),
            format!(
                "check [github] app_id and that the App is installed on {repo} with contents access"
            ),
        ),
        None => Check::skipped(GROUP, "token", "needs openssl and a readable App key"),
    }
}

fn protection_check(branch: Option<&str>, view: &GithubView) -> Check {
    let name = "branch protection";
    let Some(branch) = branch else {
        return Check::skipped(GROUP, name, "needs the default branch");
    };
    match &view.protection {
        Some(Ok(true)) => Check::ok(
            GROUP,
            name,
            format!("{branch} is protected (whether the App can bypass it: not checked)"),
        ),
        Some(Ok(false)) => Check::warn(
            GROUP,
            name,
            format!("{branch} is not protected, so the App token could push to it"),
            format!("turn on branch protection for {branch} in the repository settings"),
        ),
        Some(Err(e)) => Check::skipped(GROUP, name, format!("not checked ({e})")),
        None => Check::skipped(GROUP, name, "not checked"),
    }
}

/// `3 commits behind the default branch tip`, or why that is unknown.
#[must_use]
pub fn behind_text(view: &GithubView, base: &str) -> String {
    match view.behind.get(base) {
        Some(Ok(0)) => "at the default branch tip".to_string(),
        Some(Ok(1)) => "1 commit behind the default branch tip".to_string(),
        Some(Ok(n)) => format!("{n} commits behind the default branch tip"),
        Some(Err(e)) => format!("behind: unknown ({e})"),
        None => "behind: unknown (GitHub compare not available)".to_string(),
    }
}
