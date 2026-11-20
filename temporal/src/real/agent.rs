//! `implement` and `review`: agent jobs in this host's worktrees
//! (`real::runner`).
//!
//! Implement always starts from the lane, not from the heartbeat: the
//! remote head of the slice branch is the ground truth (heartbeat details
//! lag the pushed head). Its commit is pushed with a fence, so an attempt
//! the server already gave up on cannot overwrite a later one.

use std::path::Path;
use std::time::Duration;

use temporalio_sdk::activities::{ActivityContext, ActivityError};

use crate::classify::{FailureClass, to_activity_error};
use crate::config::{Target, expand};
use crate::real::git::{Pushed, fetch, git_ok, infra, push_fenced, remote_head};
use crate::real::runner::{Job, agent_failure, run_job};
use crate::types::{Checkpoint, ImplementInput, ReviewResult, StepInput};

/// Bring the warm repository's view of the lane up to date and return the
/// slice branch's remote head and current main.
async fn sync_repo(
    repo: &Path,
    target: &Target,
    branch: &str,
) -> Result<(Option<String>, String), ActivityError> {
    let head = remote_head(repo, &target.remote, branch).await?;
    let mut specs = vec![format!(
        "+refs/heads/{0}:refs/remotes/lane/{0}",
        target.main
    )];
    if head.is_some() {
        specs.push(format!("+refs/heads/{branch}:refs/remotes/lane/{branch}"));
    }
    fetch(repo, &target.remote, &specs).await?;
    let main_ref = format!("refs/remotes/lane/{}", target.main);
    let main = git_ok(repo, &["rev-parse", &main_ref]).await?;
    Ok((head, main))
}

pub async fn implement(
    target: &Target,
    ctx: &ActivityContext,
    input: &ImplementInput,
    host: &str,
    slots: usize,
) -> Result<Checkpoint, ActivityError> {
    let agent = &target.agent;
    let repo = expand(&agent.repo);
    let branch = &input.branch;
    let (head, main) = sync_repo(&repo, target, branch).await?;
    let beat = Checkpoint {
        branch: branch.clone(),
        head: head.clone().unwrap_or_default(),
        steps_done: input.round,
        host: host.to_string(),
    };
    let job = Job {
        subject: format!("{} {} round {}", input.need, input.slice, input.round),
        forbid: vec![input.need.clone(), branch.clone()],
        brief: implement_brief(input, target, &main),
        from: head.clone().unwrap_or_else(|| main.clone()),
        read_only: false,
        budget: Duration::from_secs(agent.budget_s),
        ctx,
    };
    let ran = run_job(agent, slots, job, &beat).await?;
    // Push what the agent committed even when it failed: the next attempt
    // starts from it.
    let pushed = match &ran.sha {
        Some(sha) => publish_wip(&repo, target, branch, sha, head.as_deref()).await?,
        None => head.clone(),
    };
    if !ran.agent_ok() {
        return Err(agent_failure("implement", &ran));
    }
    if ran.sha.is_none() {
        let what = "implement: the agent finished without a change".to_string();
        return Err(to_activity_error(&FailureClass::Format, what));
    }
    let Some(head) = pushed else {
        return Err(infra("implement: no head after the push".to_string()));
    };
    let done = Checkpoint { head, ..beat };
    ctx.record_heartbeat(done.clone()).await?;
    Ok(done)
}

/// Push the agent's commit. A fenced push means another attempt owns the
/// branch now; this attempt fails as infra and the retry starts from the
/// lane's head.
async fn publish_wip(
    repo: &Path,
    target: &Target,
    branch: &str,
    sha: &str,
    expected: Option<&str>,
) -> Result<Option<String>, ActivityError> {
    match push_fenced(repo, &target.remote, sha, branch, expected).await? {
        Pushed::Accepted => Ok(Some(sha.to_string())),
        Pushed::Fenced(text) => Err(infra(format!(
            "push of {sha} to {branch} fenced out (expected {expected:?}): {text}"
        ))),
    }
}

pub fn implement_brief(input: &ImplementInput, target: &Target, main: &str) -> String {
    let mut brief = format!(
        "Work on slice `{}` of need `{}` (round {}).\n\n{}\n",
        input.slice, input.need, input.round, input.brief
    );
    if let Some(feedback) = &input.feedback {
        brief.push_str(&format!(
            "\nThe last round came back with this. Fix it:\n\n```\n{}\n```\n",
            crate::real::cmd::tail(feedback, 4_000)
        ));
    }
    brief.push_str(&format!(
        "\nRules:\n- Keep the change small and inside the slice.\n\
         - The runner commits your changes; do not commit or push yourself.\n\
         - Before you finish, write the commit message to the file `{}` in the \
         repository root: a subject line of at most 72 characters saying what the \
         change does, for a reader of the public history, then a blank line and a \
         short body. Never name needs, handles, slices, rounds or branches in it.\n\
         - The proof is `{}`; run it before you finish.\n\
         - Current main is {main}.\n",
        crate::real::runner::MESSAGE_FILE,
        target.check_command
    ));
    brief
}

pub async fn review(
    target: &Target,
    ctx: &ActivityContext,
    input: &StepInput,
    slots: usize,
) -> Result<ReviewResult, ActivityError> {
    let agent = &target.agent;
    let repo = expand(&agent.repo);
    let (_, main) = sync_repo(&repo, target, &input.branch).await?;
    let base = git_ok(&repo, &["merge-base", &main, &input.head]).await?;
    let range = format!("{base}..{}", input.head);
    let diff = git_ok(&repo, &["diff", "--stat", "--patch", &range]).await?;
    let job = Job {
        subject: format!(
            "review {} {} round {}",
            input.need, input.slice, input.round
        ),
        forbid: Vec::new(),
        brief: review_brief(input, &base, &diff),
        from: input.head.clone(),
        read_only: true,
        budget: Duration::from_secs(agent.budget_s.min(900)),
        ctx,
    };
    let ran = run_job(agent, slots, job, &input.head).await?;
    if !ran.agent_ok() {
        return Err(agent_failure("review", &ran));
    }
    parse_verdict(&ran.receipt).ok_or_else(|| {
        let what = format!(
            "review: no verdict line in: {}",
            crate::real::cmd::tail(&ran.receipt, 400)
        );
        to_activity_error(&FailureClass::Format, what)
    })
}

/// Most of a diff a review brief carries; the rest is cut with a note.
const REVIEW_DIFF_MAX: usize = 60_000;

/// The review brief carries the diff itself, so a reviewer with no shell
/// (the read-only review hosts) can judge it from the brief and the files.
pub fn review_brief(input: &StepInput, base: &str, diff: &str) -> String {
    format!(
        "Review the change for slice `{}` of need `{}`: `git diff {base}..{}`, below.\n\n\
         What the slice was asked to do:\n\n{}\n\n\
         The machine check already passed. Judge only whether the change does what was \
         asked, correctly and without unrelated edits. The worktree is at the change's \
         head; read files there for context. Do not change any file, and do not build \
         or run tests.\n\n\
         ```diff\n{}\n```\n\n\
         End your reply with exactly one line:\n\
         `VERDICT: APPROVE` or `VERDICT: CHANGES: <what to change, one line>`\n",
        input.slice,
        input.need,
        input.head,
        input.brief,
        head(diff, REVIEW_DIFF_MAX)
    )
}

/// The first `max` bytes of `text`, with a note when it was cut.
fn head(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let cut = text.len() - end;
    format!("{}\n… ({cut} more bytes cut; read the files)", &text[..end])
}

/// The last `VERDICT:` line of a review reply.
pub fn parse_verdict(receipt: &str) -> Option<ReviewResult> {
    let line = receipt
        .lines()
        .rev()
        .map(|l| l.trim().trim_matches('`').trim())
        .find(|l| l.starts_with("VERDICT:"))?;
    let rest = line.trim_start_matches("VERDICT:").trim();
    if rest.eq_ignore_ascii_case("APPROVE") {
        return Some(ReviewResult::Approve);
    }
    let text = rest.strip_prefix("CHANGES:")?.trim();
    Some(ReviewResult::Changes {
        text: text.to_string(),
    })
}

#[cfg(test)]
#[path = "agent_test.rs"]
mod tests;
