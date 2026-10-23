//! The pool job that runs one rung command with no model.
//!
//! A rung job is read-only: the pool checks out `tip`, runs the configured
//! rung command as the harness, and the exit status is the verdict.

use super::JobSpec;
use crate::pump::config::DEFAULT_RUNG_TIMEOUT;
use crate::pump::host::Ctx;

/// Role named on a rung job. The job carries a preset step task, so nothing is
/// sent to it; the name only labels the job.
const RUNG_ROLE: &str = "pump-rung";

/// The job that runs rung `rung` for `target` against `tip` (attempt `attempt`,
/// so a retry at the same tip is a new job), or none when that rung is not
/// configured. Settings come from `PumpConfig::effective_rungs`.
#[must_use]
pub fn spec(
    ctx: &Ctx,
    target: &str,
    rung: &str,
    tip: &str,
    step_task: &str,
    attempt: u32,
) -> Option<JobSpec> {
    let effective = ctx.cfg.effective_rungs(Some(target));
    let command = effective.command(rung)?;
    let timeout = effective.timeout.unwrap_or(DEFAULT_RUNG_TIMEOUT);
    let pools = &effective.pools;
    let mut job = JobSpec::new(
        format!("rung-{rung}-{step_task}-a{attempt}"),
        "rung",
        RUNG_ROLE,
        format!("{rung} rung on {target} at {tip}"),
        format!("Run the `{rung}` rung for target {target} at {tip}.\n\n$ {command}\n"),
    );
    job.command = Some(command.to_owned());
    job.read_only = true;
    job.budget = timeout;
    job.pool_candidates = pools.clone();
    job.target = Some(target.to_owned());
    job.from_ref = Some(tip.to_owned());
    job.payload = serde_json::json!({ "rung": rung, "tip": tip, "target": target });
    Some(job)
}

#[cfg(test)]
#[path = "rung_test.rs"]
mod tests;
