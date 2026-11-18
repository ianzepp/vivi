//! One agent job through Vivi's pool machinery on this host:
//! `vivi task send` (the brief), `vivi pool dispatch` (a slot, a worktree
//! of the warm repository, the harness), `vivi pool wait` (collect), and
//! `vivi task show --latest` (the collected record and the receipt). The
//! slot, worktree, harness and commit are the pool's; this module only
//! drives it and reads the result.

use std::time::Duration;

use serde_json::Value;
use temporalio_sdk::activities::{ActivityContext, ActivityError};

use crate::classify::{classify_agent, classify_plumbing, to_activity_error};
use crate::config::{AgentConfig, expand};
use crate::real::cmd::{Cmd, Output, run};
use crate::real::git::infra;

const QUICK: Duration = Duration::from_secs(120);

/// What the pool recorded when it collected a job.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Collected {
    /// `committed`, `no_changes`, `read_only_unchanged`, …
    pub push: String,
    pub sha: String,
    pub agent_rc: String,
    pub outcome: String,
    /// The agent's hand-in or last message.
    pub receipt: String,
    /// The runner's log tail, for classification.
    pub log: String,
}

impl Collected {
    pub fn agent_ok(&self) -> bool {
        self.agent_rc == "0" && self.outcome == "finished"
    }
}

/// A job to run: the task brief and the dispatch flags beyond the pool's.
pub struct Job<'a> {
    pub subject: String,
    pub brief: String,
    pub flags: Vec<String>,
    pub budget: Duration,
    pub ctx: &'a ActivityContext,
}

fn vivi(agent: &AgentConfig, args: &[&str], timeout: Duration) -> Cmd {
    let mut cmd = Cmd::new(&agent.vivi, args, timeout).cwd(expand(&agent.space));
    cmd.args.push("--project".to_string());
    cmd.args.push(expand(&agent.space).display().to_string());
    cmd
}

/// Run a vivi command that must succeed; failures are plumbing (infra
/// unless the text says provider).
async fn vivi_ok(agent: &AgentConfig, args: &[&str]) -> Result<String, ActivityError> {
    let cmd = vivi(agent, args, QUICK);
    let out = run(&cmd).await.map_err(|e| infra(e.to_string()))?;
    if out.ok() {
        return Ok(out.stdout);
    }
    let what = format!("{}: {}", cmd.display(), out.tail(600));
    Err(to_activity_error(&classify_plumbing(&out.text()), what))
}

/// Send the brief as a task in the host's mailspace; returns its handle.
async fn send_task(
    agent: &AgentConfig,
    subject: &str,
    brief: &str,
) -> Result<String, ActivityError> {
    let args = [
        "task",
        "send",
        "--from",
        &agent.from,
        "--to",
        &agent.to,
        "--subject",
        subject,
        "--body",
        brief,
    ];
    let out = vivi_ok(agent, &args).await?;
    parse_sent(&out).ok_or_else(|| infra(format!("vivi task send printed no handle: {out}")))
}

/// The handle from `vivi task send` output (`sent 7fe3140e`).
pub fn parse_sent(out: &str) -> Option<String> {
    out.lines()
        .find_map(|l| l.strip_prefix("sent "))
        .map(|h| h.trim().to_string())
}

/// Dispatch, wait while heartbeating `beat`, and read the result.
pub async fn run_job<T>(
    agent: &AgentConfig,
    job: Job<'_>,
    beat: &T,
) -> Result<Collected, ActivityError>
where
    T: temporalio_common::data_converters::TemporalSerializable + Clone + 'static,
{
    let task = send_task(agent, &job.subject, &job.brief).await?;
    let budget = job.budget.as_secs().to_string();
    let mut args = vec![
        "pool",
        "dispatch",
        task.as_str(),
        "--pool",
        agent.pool.as_str(),
    ];
    args.extend(job.flags.iter().map(String::as_str));
    args.extend(["--budget", budget.as_str(), "--porcelain"]);
    vivi_ok(agent, &args).await?;
    let limit = (job.budget + Duration::from_secs(300))
        .as_secs()
        .to_string();
    let wait_args = [
        "pool",
        "wait",
        &task,
        "--pool",
        &agent.pool,
        "--timeout",
        &limit,
        "--porcelain",
    ];
    let wait = vivi(agent, &wait_args, job.budget + Duration::from_secs(360));
    let out = wait_beating(job.ctx, &wait, beat).await?;
    if !out.ok() {
        let what = format!("vivi pool wait {task}: {}", out.tail(600));
        return Err(to_activity_error(&classify_plumbing(&out.text()), what));
    }
    let shown = vivi_ok(agent, &["task", "show", &task, "--latest"]).await?;
    parse_collected(&shown).ok_or_else(|| infra(format!("no pool_collected record on {task}")))
}

/// Run `cmd`, heartbeating every third of the heartbeat timeout. A
/// cancelled activity (the server gave up on this attempt) stops waiting.
async fn wait_beating<T>(
    ctx: &ActivityContext,
    cmd: &Cmd,
    beat: &T,
) -> Result<Output, ActivityError>
where
    T: temporalio_common::data_converters::TemporalSerializable + Clone + 'static,
{
    let every = ctx
        .info()
        .heartbeat_timeout
        .map_or(Duration::from_secs(20), |t| t / 3)
        .max(Duration::from_secs(1));
    let running = run(cmd);
    tokio::pin!(running);
    loop {
        tokio::select! {
            out = &mut running => return out.map_err(|e| infra(e.to_string())),
            () = tokio::time::sleep(every) => {
                if ctx.is_cancelled() {
                    return Err(ActivityError::cancelled());
                }
                ctx.record_heartbeat(beat.clone()).await?;
            }
        }
    }
}

/// Read the `pool_collected` letter `vivi task show --latest` prints: the
/// record JSON in its front matter, then the receipt and the log tail.
pub fn parse_collected(shown: &str) -> Option<Collected> {
    let line = shown
        .lines()
        .find(|l| l.starts_with("record = '") && l.contains("\"outcome\""))?;
    let json = line.strip_prefix("record = '")?.strip_suffix('\'')?;
    let record: Value = serde_json::from_str(json).ok()?;
    let field = |k: &str| {
        record
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    Some(Collected {
        push: field("push"),
        sha: field("sha"),
        agent_rc: field("agent_rc"),
        outcome: field("outcome"),
        receipt: section(shown, "--- receipt"),
        log: section(shown, "--- runner log tail"),
    })
}

/// The text after a `--- name` line, up to the next `--- ` line.
fn section(text: &str, header: &str) -> String {
    let mut lines = text.lines().skip_while(|l| l.trim() != header).skip(1);
    let mut out = Vec::new();
    for line in lines.by_ref() {
        if line.starts_with("--- ") {
            break;
        }
        out.push(line);
    }
    out.join("\n").trim().to_string()
}

/// The failure for a job whose agent did not finish cleanly.
pub fn agent_failure(step: &str, c: &Collected) -> ActivityError {
    let text = format!("{}\n{}", c.receipt, c.log);
    let what = format!(
        "{step}: agent outcome {} rc {} push {}: {}",
        c.outcome,
        c.agent_rc,
        c.push,
        crate::real::cmd::tail(&text, 600)
    );
    to_activity_error(&classify_agent(&text), what)
}

#[cfg(test)]
#[path = "pool_test.rs"]
mod tests;
