//! One agent job on this host, run by the activity itself: claim a warm
//! worktree, reset it to the starting commit, run the harness with a time
//! budget while heartbeating, then commit what it changed.
//!
//! Temporal decides how many agent jobs a host runs at once (the agent
//! queue's activity slots), so the worktrees are just that many directories
//! under `slots_dir`, each claimed with a lock file for the length of one
//! job. A worktree keeps its `target/` between jobs, so builds stay warm.
//!
//! The harness runs in its own process group. At the budget, or when the
//! server cancels the attempt, the whole group is killed: the agent and
//! everything it started (a build, a test run) stop with it.

use std::fs::{File, OpenOptions};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use temporalio_sdk::activities::{ActivityContext, ActivityError};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::classify::{classify_agent, to_activity_error};
use crate::config::{AgentConfig, expand};
use crate::real::cmd::tail;
use crate::real::git::{git, git_ok, infra};

/// One job to run.
pub struct Job<'a> {
    /// The job's name in errors and logs. Never a commit message: commits
    /// are public history and must not name needs or branches.
    pub subject: String,
    /// Words a commit message must not contain (the need's handle, the
    /// branch). A message that has one is replaced by `FALLBACK_MESSAGE`.
    pub forbid: Vec<String>,
    pub brief: String,
    /// The commit the worktree starts from.
    pub from: String,
    /// Review: the read-only harness, and nothing is committed.
    pub read_only: bool,
    pub budget: Duration,
    pub ctx: &'a ActivityContext,
}

/// What a job left behind.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ran {
    /// The harness's exit code; `None` when it was killed at the budget.
    pub rc: Option<i32>,
    /// The harness's standard output: its final reply.
    pub receipt: String,
    /// The tail of its standard error.
    pub log: String,
    /// The commit made from the agent's changes; `None` when it changed
    /// nothing or the job was read-only.
    pub sha: Option<String>,
}

impl Ran {
    pub fn agent_ok(&self) -> bool {
        self.rc == Some(0)
    }
}

/// The failure for a job whose harness did not exit cleanly.
pub fn agent_failure(step: &str, ran: &Ran) -> ActivityError {
    let text = format!("{}\n{}", ran.receipt, ran.log);
    let rc = ran
        .rc
        .map_or("killed at the budget".to_string(), |rc| format!("rc {rc}"));
    let what = format!("{step}: agent {rc}: {}", tail(&text, 600));
    to_activity_error(&classify_agent(&text), what)
}

/// The harness command for a job, with `{model}` and `{brief}` filled in.
pub fn harness_command(agent: &AgentConfig, read_only: bool, brief: &Path) -> String {
    let template = if read_only {
        &agent.review_harness
    } else {
        &agent.harness
    };
    template
        .replace("{model}", &agent.model)
        .replace("{brief}", &brief.display().to_string())
}

/// Run `job` in one of this host's `slots` worktrees, heartbeating `beat`.
pub async fn run_job<T>(
    agent: &AgentConfig,
    slots: usize,
    job: Job<'_>,
    beat: &T,
) -> Result<Ran, ActivityError>
where
    T: temporalio_common::data_converters::TemporalSerializable + Clone + 'static,
{
    let dir = expand(&agent.slots_dir);
    let (n, _lock) = claim_waiting(&dir, slots).await?;
    let tree = dir.join(format!("slot-{n}"));
    let repo = expand(&agent.repo);
    reset(&repo, &tree, &job.from).await?;
    let brief = dir.join(format!("slot-{n}.brief.md"));
    std::fs::write(&brief, &job.brief).map_err(|e| infra(format!("{}: {e}", brief.display())))?;
    let command = harness_command(agent, job.read_only, &brief);
    let mut ran = run_harness(&command, &tree, job.budget, job.ctx, beat).await?;
    let message = take_message(&tree, &job.forbid);
    if !job.read_only {
        ran.sha = commit(&tree, &message).await?;
    }
    Ok(ran)
}

/// The file the agent writes its commit message to, at the worktree root.
/// The runner reads and removes it before committing.
pub const MESSAGE_FILE: &str = ".commit-message";

/// The commit message when the agent wrote none, or wrote one that names
/// something it must not.
pub const FALLBACK_MESSAGE: &str = "Agent change without a commit message";

/// Read and remove the agent's commit message; fall back when it is
/// missing, empty, or contains a forbidden word.
pub fn take_message(tree: &Path, forbid: &[String]) -> String {
    let path = tree.join(MESSAGE_FILE);
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let _ = std::fs::remove_file(&path);
    let text = text.trim();
    let named = forbid
        .iter()
        .any(|w| !w.is_empty() && text.contains(w.as_str()));
    if text.is_empty() || named {
        return FALLBACK_MESSAGE.to_string();
    }
    text.to_string()
}

/// Lock the first free `slot-<n>.lock` under `dir`, 1..=slots. The lock is
/// held while the returned file lives.
pub fn claim(dir: &Path, slots: usize) -> Result<(usize, File), ActivityError> {
    std::fs::create_dir_all(dir).map_err(|e| infra(format!("{}: {e}", dir.display())))?;
    for n in 1..=slots {
        let path = dir.join(format!("slot-{n}.lock"));
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|e| infra(format!("{}: {e}", path.display())))?;
        if file.try_lock().is_ok() {
            return Ok((n, file));
        }
    }
    Err(infra(format!(
        "all {slots} worktrees under {} are in use",
        dir.display()
    )))
}

/// `claim`, retried for a few seconds. A lock just released can still look
/// held for a moment: a child process spawned by another job inherits the
/// lock file until it execs (the file is close-on-exec).
pub async fn claim_waiting(dir: &Path, slots: usize) -> Result<(usize, File), ActivityError> {
    let mut last = None;
    for _ in 0..10 {
        match claim(dir, slots) {
            Ok(claimed) => return Ok(claimed),
            Err(err) => last = Some(err),
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err(last.expect("at least one try"))
}

/// Make `tree` a clean detached checkout of `from`, keeping `target/`.
async fn reset(repo: &Path, tree: &Path, from: &str) -> Result<(), ActivityError> {
    let path = tree.display().to_string();
    if !tree.join(".git").exists() {
        let _ = git(repo, &["worktree", "prune"]).await;
        git_ok(repo, &["worktree", "add", "-q", "--detach", &path, from]).await?;
        return Ok(());
    }
    git_ok(tree, &["checkout", "-q", "-f", "--detach", from]).await?;
    git_ok(tree, &["clean", "-fdq", "-e", "target"]).await?;
    Ok(())
}

/// Commit everything the agent changed; `None` when it changed nothing.
async fn commit(tree: &Path, subject: &str) -> Result<Option<String>, ActivityError> {
    git_ok(tree, &["add", "-A"]).await?;
    if git(tree, &["diff", "--cached", "--quiet"]).await?.ok() {
        return Ok(None);
    }
    git_ok(tree, &["commit", "-q", "--no-verify", "-m", subject]).await?;
    Ok(Some(git_ok(tree, &["rev-parse", "HEAD"]).await?))
}

/// Run the harness under `sh -c` in its own process group, heartbeating
/// every third of the heartbeat timeout. The group is killed at the budget
/// or on cancel.
async fn run_harness<T>(
    command: &str,
    tree: &Path,
    budget: Duration,
    ctx: &ActivityContext,
    beat: &T,
) -> Result<Ran, ActivityError>
where
    T: temporalio_common::data_converters::TemporalSerializable + Clone + 'static,
{
    let mut child = Command::new("sh")
        .args(["-c", command])
        .current_dir(tree)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| infra(format!("spawn harness: {e}")))?;
    let group = child.id();
    let _killer = GroupKiller(group);
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let out = tokio::spawn(async move {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s).await;
        s
    });
    let err = tokio::spawn(async move {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s).await;
        s
    });
    let every = ctx
        .info()
        .heartbeat_timeout
        .map_or(Duration::from_secs(20), |t| t / 3)
        .max(Duration::from_secs(1));
    let deadline = tokio::time::Instant::now() + budget;
    let rc = loop {
        tokio::select! {
            status = child.wait() => {
                break status.map_err(|e| infra(format!("harness: {e}")))?.code();
            }
            () = tokio::time::sleep_until(deadline) => {
                kill_group(group);
                let _ = child.wait().await;
                break None;
            }
            () = tokio::time::sleep(every) => {
                if ctx.is_cancelled() {
                    kill_group(group);
                    return Err(ActivityError::cancelled());
                }
                ctx.record_heartbeat(beat.clone()).await?;
            }
        }
    };
    let receipt = out.await.unwrap_or_default();
    let log = err.await.unwrap_or_default();
    Ok(Ran {
        rc,
        receipt: receipt.trim().to_string(),
        log: tail(&log, 4_000),
        sha: None,
    })
}

/// Kills the harness's process group when dropped, so a job abandoned for
/// any reason (an error, a dropped future) leaves nothing running.
struct GroupKiller(Option<u32>);

impl Drop for GroupKiller {
    fn drop(&mut self) {
        kill_group(self.0);
    }
}

/// SIGKILL the process group led by `pid` (a no-op once it is gone).
fn kill_group(pid: Option<u32>) {
    if let Some(pid) = pid.and_then(|p| libc::pid_t::try_from(p).ok()) {
        // SAFETY: killpg only sends a signal; a stale group id yields ESRCH.
        unsafe {
            libc::killpg(pid, libc::SIGKILL);
        }
    }
}

#[cfg(test)]
#[path = "runner_test.rs"]
mod tests;
