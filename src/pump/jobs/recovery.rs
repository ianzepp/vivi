//! Reattach task handles and pool receipts after a pump restart.

use std::collections::BTreeMap;

use crate::error::VivariumError;
use crate::pump::host::{Ctx, shell_quote};

use super::{JobOutcome, JobStatus, JobStore, PoolJob, tail};

/// Recover task handles and dispatch states written after the last pump save.
///
/// The task subject carries a stable job ID. If the pump died after `task
/// send` but before saving its handle, the task list finds that same task and
/// prevents a duplicate send. A terminal `pool result` is saved for the next
/// normal state write.
pub fn recover_dispatched(ctx: &Ctx, st: &mut impl JobStore) -> Result<(), VivariumError> {
    let mut pending_by_role = BTreeMap::<String, Vec<String>>::new();
    let existing: Vec<(String, String)> = st
        .jobs()
        .iter()
        .filter(|(_, job)| job.status == JobStatus::Pending || job.status == JobStatus::Dispatched)
        .filter_map(|(id, job)| job.task.as_ref().map(|task| (id.clone(), task.clone())))
        .collect();
    for (id, task) in existing {
        let latest = crate::pump::collect::receipt_full(ctx, &task);
        recover_task(ctx, st, &id, &task, &latest);
    }
    pending_without_task(st, &mut pending_by_role);
    for (role, job_ids) in pending_by_role {
        recover_role_tasks(ctx, st, &role, &job_ids)?;
    }
    Ok(())
}

fn pending_without_task(st: &impl JobStore, by_role: &mut BTreeMap<String, Vec<String>>) {
    for (id, job) in st.jobs() {
        if job.status == JobStatus::Pending && job.task.is_none() {
            by_role
                .entry(job.role.clone())
                .or_default()
                .push(id.clone());
        }
    }
}

fn recover_role_tasks(
    ctx: &Ctx,
    st: &mut impl JobStore,
    role: &str,
    job_ids: &[String],
) -> Result<(), VivariumError> {
    let listed = ctx.vivi(
        &format!("task list --for {} --status all --json", shell_quote(role)),
        120,
    );
    if listed.code != 0 {
        return Err(VivariumError::Message(format!(
            "cannot recover pump jobs for role {role}: {}",
            tail(&listed.text, 300)
        )));
    }
    for (task, row) in listed_tasks(&listed.text)? {
        if all_recovered(st.jobs(), job_ids) {
            break;
        }
        let Some(id) = job_ids.iter().find(|id| marker_matches(&row, id)) else {
            continue;
        };
        let shown = ctx.vivi(&format!("task show {task} --json"), 60);
        if shown.code != 0 {
            return Err(VivariumError::Message(format!(
                "cannot inspect task {task} while recovering pump jobs: {}",
                tail(&shown.text, 240)
            )));
        }
        if !marker_matches(&shown.text, id) {
            return Err(VivariumError::Message(format!(
                "task {task} subject matched pump job {id}, but its body marker is missing"
            )));
        }
        let latest = crate::pump::collect::receipt_full(ctx, &task);
        recover_task(ctx, st, id, &task, &latest);
    }
    Ok(())
}

/// `true` when each job for this role has a recovered handle or state.
fn all_recovered(jobs: &BTreeMap<String, PoolJob>, ids: &[String]) -> bool {
    ids.iter().all(|id| {
        jobs.get(id)
            .is_some_and(|job| job.task.is_some() || job.status != JobStatus::Pending)
    })
}

pub(super) fn recover_task(ctx: &Ctx, st: &mut impl JobStore, id: &str, task: &str, latest: &str) {
    let Some(job) = st.jobs_mut().get_mut(id) else {
        return;
    };
    if job.task.is_none() {
        job.task = Some(task.to_owned());
    }
    if let Some((status, detail)) = pool_result(latest) {
        let outcome = JobOutcome {
            state: status.as_str().to_owned(),
            detail,
            receipt: latest.to_owned(),
            at: ctx.now(),
        };
        job.status = status;
        job.outcome = Some(outcome);
    } else if latest
        .lines()
        .any(|line| line.starts_with("pool dispatch "))
    {
        job.status = JobStatus::Dispatched;
        job.pool = pool_name(latest);
        job.dispatched_at.get_or_insert_with(|| ctx.now());
    }
}

fn pool_result(latest: &str) -> Option<(JobStatus, String)> {
    let line = latest
        .lines()
        .find(|line| line.starts_with("pool result "))?;
    let (_, state) = line.split_once(':')?;
    let mut words = state.split_whitespace();
    let status = match words.next()? {
        "finished" => JobStatus::Finished,
        "timeout" => JobStatus::Timeout,
        "lost" => JobStatus::Lost,
        _ => return None,
    };
    let detail = latest
        .lines()
        .find(|line| line.starts_with("push:") || line.starts_with("result:"))
        .map_or_else(|| line.trim().to_owned(), |line| line.trim().to_owned());
    Some((status, detail))
}

fn pool_name(latest: &str) -> Option<String> {
    latest.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        if words.next() != Some("pool") {
            return None;
        }
        let name = words.next()?;
        (!matches!(name, "dispatch" | "result")).then(|| name.to_owned())
    })
}

fn marker_matches(text: &str, id: &str) -> bool {
    text.contains(&format!("pump-job-id: {id}")) || text.contains(&format!("[pump-job:{id}]"))
}

fn listed_tasks(text: &str) -> Result<Vec<(String, String)>, VivariumError> {
    let value = serde_json::from_str::<serde_json::Value>(text).map_err(|err| {
        VivariumError::Message(format!(
            "task list returned invalid JSON during job recovery: {err}"
        ))
    })?;
    let mut rows = Vec::new();
    collect_tasks(&value, &mut rows);
    rows.sort_by(|left, right| left.0.cmp(&right.0));
    rows.dedup_by(|left, right| left.0 == right.0);
    Ok(rows)
}

fn collect_tasks(value: &serde_json::Value, tasks: &mut Vec<(String, String)>) {
    match value {
        serde_json::Value::Object(object) => {
            if let (Some(handle), Some(subject)) = (
                object.get("handle").and_then(serde_json::Value::as_str),
                object.get("subject").and_then(serde_json::Value::as_str),
            ) {
                tasks.push((handle.to_owned(), format!("{subject}\n{value}")));
                return;
            }
            for child in object.values() {
                collect_tasks(child, tasks);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_tasks(item, tasks);
            }
        }
        _ => {}
    }
}
