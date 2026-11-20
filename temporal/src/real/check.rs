//! `check`: the pre-queue machine check, in this host's verification tree
//! (shared with the gate, never at the same time: one verify queue slot). It
//! checks the slice head merged with current main (the main it will join),
//! then runs the target's check command. Red is a value; only a check that
//! could not run is an error.

use std::path::Path;
use std::time::Duration;

use temporalio_sdk::activities::ActivityError;

use crate::classify::{FailureClass, classify_check, to_activity_error};
use crate::config::{Target, expand};
use crate::real::cmd::{Cmd, run};
use crate::real::git::{IDENT, fetch, git, git_ok, infra};
use crate::types::{CheckResult, StepInput};

pub async fn check(target: &Target, input: &StepInput) -> Result<CheckResult, ActivityError> {
    let tree = expand(&target.verify_tree);
    let specs = vec![
        format!("+refs/heads/{0}:refs/remotes/lane/{0}", target.main),
        format!("+refs/heads/{0}:refs/remotes/lane/{0}", input.branch),
    ];
    fetch(&tree, &target.remote, &specs).await?;
    let main_ref = format!("refs/remotes/lane/{}", target.main);
    let main = git_ok(&tree, &["rev-parse", &main_ref]).await?;
    git_ok(&tree, &["checkout", "-q", "-f", "--detach", &input.head]).await?;
    git_ok(&tree, &["clean", "-fdq", "-e", "target"]).await?;
    if let Some(red) = merge_main(&tree, &main).await? {
        return Ok(red);
    }
    let mut cmd = Cmd::new(
        "sh",
        &["-c", &target.check_command],
        Duration::from_secs(target.check_timeout_s),
    )
    .cwd(tree)
    .env("VIVI_BASE", &main)
    .env("VIVI_HEAD", &input.head);
    for (key, value) in IDENT {
        cmd = cmd.env(key, value);
    }
    let out = run(&cmd).await.map_err(|e| infra(e.to_string()))?;
    if out.ok() {
        return Ok(CheckResult::Green);
    }
    let text = format!(
        "check `{}` failed at {} on main {}:\n{}",
        target.check_command,
        short(&input.head),
        short(&main),
        out.tail(3_000)
    );
    match classify_check(out.rc, &out.text()) {
        FailureClass::Work => Ok(CheckResult::Red { text }),
        class => Err(to_activity_error(&class, text)),
    }
}

/// Merge current main into the detached head. `Some(red)` when it does not
/// merge; the tree is left clean either way.
async fn merge_main(tree: &Path, main: &str) -> Result<Option<CheckResult>, ActivityError> {
    let merged = git(tree, &["merge", "-q", "--no-ff", "--no-edit", main]).await?;
    if merged.ok() {
        return Ok(None);
    }
    let files = git_ok(tree, &["diff", "--name-only", "--diff-filter=U"]).await?;
    let _ = git(tree, &["merge", "--abort"]).await;
    if files.is_empty() {
        return Err(infra(format!("git merge {main}: {}", merged.tail(600))));
    }
    let list: Vec<&str> = files.lines().collect();
    Ok(Some(CheckResult::Red {
        text: format!(
            "the slice does not merge with current main {}: conflicts in {}",
            short(main),
            list.join(", ")
        ),
    }))
}

fn short(sha: &str) -> &str {
    sha.get(..10).unwrap_or(sha)
}
