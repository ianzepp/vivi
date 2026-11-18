//! `implement` and `review`: agent jobs through the pool on this host.
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
use crate::real::pool::{Job, agent_failure, run_job};
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
) -> Result<Checkpoint, ActivityError> {
    let agent = &target.agent;
    let repo = expand(&agent.repo);
    let branch = &input.branch;
    let (head, main) = sync_repo(&repo, target, branch).await?;
    // The pool continues a local branch only from itself; point it at the
    // lane's head, or drop a stale one when the lane has none.
    let local = format!("refs/heads/{branch}");
    let from = match &head {
        Some(sha) => {
            git_ok(&repo, &["update-ref", &local, sha]).await?;
            branch.clone()
        }
        None => {
            let _ = git_ok(&repo, &["update-ref", "-d", &local]).await;
            main.clone()
        }
    };
    let beat = Checkpoint {
        branch: branch.clone(),
        head: head.clone().unwrap_or_default(),
        steps_done: input.round,
        host: host.to_string(),
    };
    let job = Job {
        subject: format!("{} {} round {}", input.need, input.slice, input.round),
        brief: implement_brief(input, target, &main),
        flags: vec![
            "--branch".into(),
            branch.clone(),
            "--from".into(),
            from,
            "--model".into(),
            agent.model.clone(),
        ],
        budget: Duration::from_secs(agent.budget_s),
        ctx,
    };
    let collected = run_job(agent, job, &beat).await?;
    let pushed = match collected.push.as_str() {
        "committed" => publish_wip(&repo, target, branch, &collected.sha, head.as_deref()).await?,
        _ => head.clone(),
    };
    if !collected.agent_ok() {
        return Err(agent_failure("implement", &collected));
    }
    let Some(head) = pushed else {
        let what = "implement: the agent finished without a change".to_string();
        return Err(to_activity_error(&FailureClass::Format, what));
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
         - The proof is `{}`; run it before you finish.\n\
         - Current main is {main}.\n",
        target.check_command
    ));
    brief
}

pub async fn review(
    target: &Target,
    ctx: &ActivityContext,
    input: &StepInput,
) -> Result<ReviewResult, ActivityError> {
    let agent = &target.agent;
    let repo = expand(&agent.repo);
    let (_, main) = sync_repo(&repo, target, &input.branch).await?;
    let base = git_ok(&repo, &["merge-base", &main, &input.head]).await?;
    let job = Job {
        subject: format!(
            "review {} {} round {}",
            input.need, input.slice, input.round
        ),
        brief: review_brief(input, &base),
        flags: vec![
            "--read-only".into(),
            "--from".into(),
            input.head.clone(),
            "--model".into(),
            agent.model.clone(),
        ],
        budget: Duration::from_secs(agent.budget_s.min(900)),
        ctx,
    };
    let collected = run_job(agent, job, &input.head).await?;
    if !collected.agent_ok() {
        return Err(agent_failure("review", &collected));
    }
    parse_verdict(&collected.receipt).ok_or_else(|| {
        let what = format!(
            "review: no verdict line in: {}",
            crate::real::cmd::tail(&collected.receipt, 400)
        );
        to_activity_error(&FailureClass::Format, what)
    })
}

pub fn review_brief(input: &StepInput, base: &str) -> String {
    format!(
        "Review the change for slice `{}` of need `{}`: `git diff {base}..{}`.\n\n\
         What the slice was asked to do:\n\n{}\n\n\
         The machine check already passed. Judge only whether the change does what was \
         asked, correctly and without unrelated edits. Do not change any file.\n\n\
         End your reply with exactly one line:\n\
         `VERDICT: APPROVE` or `VERDICT: CHANGES: <what to change, one line>`\n",
        input.slice, input.need, input.head, input.brief
    )
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
