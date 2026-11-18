//! The git the real activities need: read a lane branch, fetch into a warm
//! tree, and push with a fence. Pushes never force: a push names the head
//! it expects (`--force-with-lease=<ref>:<expected>`, empty for "must not
//! exist"), so an attempt that was declared lost cannot overwrite the work
//! a later attempt pushed.

use std::path::Path;
use std::time::Duration;

use temporalio_sdk::activities::ActivityError;

use crate::classify::{classify_plumbing, to_activity_error};
use crate::real::cmd::{Cmd, Output, run};

const GIT_TIMEOUT: Duration = Duration::from_secs(300);

/// The committer every commit the runtime makes is written as.
pub const IDENT: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "Ian Zepp"),
    ("GIT_AUTHOR_EMAIL", "ian.zepp@protonmail.com"),
    ("GIT_COMMITTER_NAME", "Ian Zepp"),
    ("GIT_COMMITTER_EMAIL", "ian.zepp@protonmail.com"),
];

/// Run git in `repo`. A failure to start is infra.
pub async fn git(repo: &Path, args: &[&str]) -> Result<Output, ActivityError> {
    let mut cmd = Cmd::new("git", &["-C", &repo.display().to_string()], GIT_TIMEOUT);
    cmd.args.extend(args.iter().map(|s| s.to_string()));
    for (key, value) in IDENT {
        cmd = cmd.env(key, value);
    }
    run(&cmd).await.map_err(|e| infra(e.to_string()))
}

/// Run git and require success; a failure is classified from its text.
pub async fn git_ok(repo: &Path, args: &[&str]) -> Result<String, ActivityError> {
    let out = git(repo, args).await?;
    if out.ok() {
        return Ok(out.stdout.trim().to_string());
    }
    let class = classify_plumbing(&out.text());
    let what = format!("git {}: {}", args.join(" "), out.tail(600));
    Err(to_activity_error(&class, what))
}

pub fn infra(message: String) -> ActivityError {
    to_activity_error(&crate::classify::FailureClass::Infra, message)
}

/// The head of `branch` on `remote`, or `None` when it does not exist.
pub async fn remote_head(
    repo: &Path,
    remote: &str,
    branch: &str,
) -> Result<Option<String>, ActivityError> {
    let refname = format!("refs/heads/{branch}");
    let out = git_ok(repo, &["ls-remote", remote, &refname]).await?;
    Ok(parse_ls_remote(&out, &refname))
}

/// The sha `ls-remote` printed for exactly `refname`.
pub fn parse_ls_remote(out: &str, refname: &str) -> Option<String> {
    out.lines().find_map(|line| {
        let (sha, name) = line.split_once('\t')?;
        (name.trim() == refname).then(|| sha.trim().to_string())
    })
}

/// Fetch `refspecs` from `remote` into `repo`.
pub async fn fetch(repo: &Path, remote: &str, refspecs: &[String]) -> Result<(), ActivityError> {
    let mut args = vec!["fetch", "-q", remote];
    args.extend(refspecs.iter().map(String::as_str));
    git_ok(repo, &args).await.map(|_| ())
}

/// How a fenced push ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pushed {
    Accepted,
    /// The branch was not at the expected head: another attempt or the
    /// Lander moved it.
    Fenced(String),
}

/// Push `sha` to `refs/heads/<branch>` on `remote` only if the branch is at
/// `expected` (`None`: only if it does not exist).
pub async fn push_fenced(
    repo: &Path,
    remote: &str,
    sha: &str,
    branch: &str,
    expected: Option<&str>,
) -> Result<Pushed, ActivityError> {
    let refname = format!("refs/heads/{branch}");
    let lease = format!("--force-with-lease={refname}:{}", expected.unwrap_or(""));
    let spec = format!("{sha}:{refname}");
    let out = git(repo, &["push", "--porcelain", &lease, remote, &spec]).await?;
    if out.ok() {
        return Ok(Pushed::Accepted);
    }
    let text = out.text();
    if is_fenced(&text) {
        return Ok(Pushed::Fenced(out.tail(400)));
    }
    let class = classify_plumbing(&text);
    Err(to_activity_error(
        &class,
        format!("git push {spec}: {}", out.tail(600)),
    ))
}

/// A push refused because the remote ref was not where we expected.
pub fn is_fenced(text: &str) -> bool {
    [
        "stale info",
        "[rejected]",
        "non-fast-forward",
        "fetch first",
        "already exists",
    ]
    .iter()
    .any(|p| text.contains(p))
}

#[cfg(test)]
#[path = "git_test.rs"]
mod tests;
