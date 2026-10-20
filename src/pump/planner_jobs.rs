//! Durable planner jobs and their local result adapter.
//!
//! Planner jobs run in pool checkouts, so they return plan JSON in the task
//! receipt. This module writes that JSON and the existing exit-receipt shape
//! into the pump home, where the plan and failure collectors already read.

use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::error::VivariumError;
use crate::mailspace::shipped::PUMP_PLANNER_ROLE;
use crate::pump::host::{Ctx, exit_receipt_path};
use crate::pump::jobs::{self, JobOutcome, JobSpec, JobStatus, JobStore, PoolJob};
use crate::pump::state::State;

const PLAN_BEGIN: &str = "PUMP-PLAN-BEGIN";
const PLAN_END: &str = "PUMP-PLAN-END";

/// Queue one planner task; the pool role owns its execution after dispatch.
pub fn enqueue(
    ctx: &Ctx,
    st: &mut State,
    kind: &str,
    owner: &str,
    queued_at: u64,
    prompt: &str,
    budget: u64,
    out_rel: &str,
    plan_rel: &str,
    target: Option<&str>,
) -> Result<String, VivariumError> {
    let (role, pool) = planner_role(ctx)?;
    let id = job_id(kind, owner, queued_at);
    let existing = st.jobs().contains_key(&id);
    if !existing {
        prepare_receipt(ctx, out_rel)?;
    }
    let body = pool_prompt(prompt, plan_rel);
    let mut payload = serde_json::json!({
        "planner_output": { "out": out_rel, "plan": plan_rel }
    });
    let mut spec = JobSpec::new(
        id.clone(),
        format!("planner:{kind}"),
        role,
        format!("{kind} planner {owner}"),
        body,
    );
    spec.pool_candidates = vec![pool];
    spec.target = target.map(str::to_owned);
    spec.from_ref = Some(start_ref(ctx, target));
    spec.budget = budget;
    spec.read_only = true;
    spec.payload = payload;
    jobs::enqueue(st, spec)?;
    ctx.log(&format!("planner job queued {id}"));
    Ok(id)
}

/// The ref a planner checks out: the target's dispatch start, else the
/// pump's own. `pool dispatch` refuses a job with no `--from`.
fn start_ref(ctx: &Ctx, target: Option<&str>) -> String {
    target.and_then(|id| ctx.cfg.for_target(id)).map_or_else(
        || ctx.cfg.dispatch_from().to_owned(),
        |cfg| cfg.dispatch_from().to_owned(),
    )
}

/// Stable job ID for one owner and enqueue time.
#[must_use]
pub fn job_id(kind: &str, owner: &str, at: u64) -> String {
    format!("planner-{kind}-{owner}-{at}")
}

/// The lifecycle and dispatch time for one owner’s planner job.
#[must_use]
pub fn lifecycle(
    st: &impl JobStore,
    kind: &str,
    owner: &str,
    queued_at: u64,
) -> Option<(JobStatus, Option<u64>)> {
    let job = st.jobs().get(&job_id(kind, owner, queued_at))?;
    Some((job.status, job.dispatched_at))
}

/// The saved terminal result for one owner’s planner job.
#[must_use]
pub fn completion<'a>(
    st: &'a impl JobStore,
    kind: &str,
    owner: &str,
    queued_at: u64,
) -> Option<&'a JobOutcome> {
    jobs::completion(st, &job_id(kind, owner, queued_at))
}

/// Status lines for queued and dispatched planner work.
#[must_use]
pub fn status_lines(ctx: &Ctx, st: &impl JobStore) -> Vec<String> {
    let now = ctx.now();
    st.jobs()
        .values()
        .filter(|job| job.kind.starts_with("planner:") && !job.status.is_terminal())
        .map(|job| {
            let queued = if job.queued_at == 0 {
                now
            } else {
                job.queued_at
            };
            let since = job.dispatched_at.unwrap_or(queued);
            let age = now.saturating_sub(since);
            let (phase, pool) = match (job.status, job.pool.as_deref()) {
                (JobStatus::Pending, _) => {
                    ("queued", job.pool_candidates.first().map(String::as_str))
                }
                (JobStatus::Dispatched, pool) => ("in-flight", pool),
                _ => ("waiting", None),
            };
            format!(
                "planner-job {} {phase} pool={} task={} age={age}s",
                job.id,
                pool.unwrap_or("unassigned"),
                job.task.as_deref().unwrap_or("pending")
            )
        })
        .collect()
}

/// Materialize saved planner receipts on every tick, including after restart.
pub fn apply_completions(ctx: &Ctx, st: &mut State) {
    let ready: Vec<(String, PoolJob)> = st
        .jobs()
        .iter()
        .filter(|(_, job)| {
            job.kind.starts_with("planner:") && job.outcome.is_some() && job.applied_at.is_none()
        })
        .map(|(id, job)| (id.clone(), job.clone()))
        .collect();
    for (id, job) in ready {
        match apply_one(ctx, &job) {
            Ok(()) => mark_applied(ctx, st, &id),
            Err(error) => ctx.log(&format!("planner result {id} could not be saved: {error}")),
        }
    }
}

fn planner_role(ctx: &Ctx) -> Result<(String, String), VivariumError> {
    let role = ctx
        .routing
        .pump_planner()
        .filter(|role| role.status == "active")
        .ok_or_else(|| {
            VivariumError::Config(format!(
                "durable planner jobs need an active {PUMP_PLANNER_ROLE} role; run `vivi config migrate`"
            ))
        })?;
    let pool = role.pool.clone().ok_or_else(|| {
        VivariumError::Config(format!(
            "role {} needs a pool for durable planner jobs",
            role.name
        ))
    })?;
    Ok((role.name.clone(), pool))
}

fn pool_prompt(prompt: &str, plan_rel: &str) -> String {
    format!(
        "{prompt}\n\nPool result contract: this worker cannot write to the pump host. Do not try to write `{plan_rel}`. Return the complete plan as JSON between these exact lines:\n{PLAN_BEGIN}\n<complete plan JSON>\n{PLAN_END}\nKeep any short status outside the markers."
    )
}

fn prepare_receipt(ctx: &Ctx, out_rel: &str) -> Result<(), VivariumError> {
    let out = local_path(ctx, out_rel)?;
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&out, "")?;
    let receipt = exit_receipt_path(&out);
    match fs::remove_file(receipt) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn apply_one(ctx: &Ctx, job: &PoolJob) -> Result<(), VivariumError> {
    let output = planner_output(&job.payload)?;
    let out = local_path(ctx, output.out)?;
    let outcome = job.outcome.as_ref().ok_or_else(|| {
        VivariumError::Message(format!("planner job {} has no terminal outcome", job.id))
    })?;
    if let Some(plan) = plan_json(&outcome.receipt) {
        atomic_write(&local_path(ctx, output.plan)?, plan.as_bytes())?;
    }
    atomic_write(&out, outcome.receipt.as_bytes())?;
    let code = planner_exit_code(&outcome.receipt, &outcome.state);
    atomic_write(&exit_receipt_path(&out), format!("{code}\n").as_bytes())
}

struct OutputPaths<'a> {
    out: &'a str,
    plan: &'a str,
}

fn planner_output(payload: &serde_json::Value) -> Result<OutputPaths<'_>, VivariumError> {
    let output = payload
        .get("planner_output")
        .ok_or_else(|| VivariumError::Message("planner job has no local output paths".into()))?;
    let out = output["out"]
        .as_str()
        .ok_or_else(|| VivariumError::Message("planner job has no receipt output path".into()))?;
    let plan = output["plan"]
        .as_str()
        .ok_or_else(|| VivariumError::Message("planner job has no plan output path".into()))?;
    Ok(OutputPaths { out, plan })
}

fn plan_json(receipt: &str) -> Option<&str> {
    let final_message = receipt
        .split_once("hand's final message:")
        .map_or(receipt, |(_, body)| body);
    let body = final_message.rsplit_once(PLAN_BEGIN)?.1;
    let plan = body.split_once(PLAN_END)?.0.trim();
    (!plan.is_empty()).then_some(plan)
}

fn planner_exit_code(receipt: &str, state: &str) -> i32 {
    receipt
        .split("hand's final message:")
        .next()
        .unwrap_or(receipt)
        .lines()
        .rev()
        .find_map(|line| {
            let line = line.trim_start();
            let value = line
                .strip_prefix("agent_rc ")
                .or_else(|| line.strip_prefix("agent_rc="))?;
            value.split_whitespace().next()?.parse().ok()
        })
        .unwrap_or(match state {
            "timeout" => 124,
            "lost" => 137,
            _ => -1,
        })
}

fn mark_applied(ctx: &Ctx, st: &mut State, id: &str) {
    if let Some(job) = st.jobs_mut().get_mut(id) {
        job.applied_at = Some(ctx.now());
    }
}

fn local_path(ctx: &Ctx, relative: &str) -> Result<PathBuf, VivariumError> {
    let path = Path::new(relative);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(VivariumError::Config(format!(
            "planner output path must stay under the pump home: {relative:?}"
        )));
    }
    Ok(ctx.home.join(path))
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), VivariumError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("planner-tmp");
    fs::write(&temp, contents)?;
    fs::rename(temp, path)?;
    Ok(())
}

#[cfg(test)]
#[path = "planner_jobs_test.rs"]
mod tests;
