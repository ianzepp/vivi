//! Dispatching pending jobs: pool selection, the concurrent task send and
//! `pool dispatch` waves, and applying their outcomes to the job store.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::Path;

use super::{JobStatus, JobStore, PoolJob, brief, recovery, tail};
use crate::error::VivariumError;
use crate::pump::dispatch;
use crate::pump::host::{Ctx, shell_quote};
use crate::pump::par::map_bounded;

/// Dispatch every pending job that has a free pool slot.
///
/// Returns IDs accepted by a pool. Jobs without capacity remain pending.
///
/// # Errors
/// Returns filesystem and task-store errors that prevent writing a brief.
pub fn dispatch_pending(ctx: &Ctx, st: &mut impl JobStore) -> Result<Vec<String>, VivariumError> {
    if !st
        .jobs()
        .values()
        .any(|job| job.status == JobStatus::Pending)
    {
        return Ok(Vec::new());
    }
    let mut free = dispatch::slots(ctx);
    let priorities = crate::pump::priority::levels(ctx);
    let mut pending: Vec<PoolJob> = st
        .jobs()
        .iter()
        .filter(|(_, job)| job.status == JobStatus::Pending)
        .map(|(_, job)| job.clone())
        .collect();
    pending.sort_by_key(|job| dispatch_order(job, &ctx.cfg.fix_prefix, &priorities));
    let mut started = Vec::new();
    let width = (ctx.cfg.dispatch.parallel as usize).max(1);
    for chunk in pending.chunks(width) {
        started.extend(dispatch_chunk(ctx, st, chunk, &mut free)?);
    }
    Ok(started)
}

/// One job being offered to its candidate pools in order.
struct Attempt {
    snapshot: PoolJob,
    remaining: VecDeque<String>,
}

/// Dispatch up to `[pump.dispatch] parallel` jobs at once. Each wave picks a
/// pool for every active job in order (first candidate with a free slot, the
/// wave's own picks counted), runs the task sends and `pool dispatch` calls
/// concurrently, then applies the outcomes in job order exactly as a serial
/// run would. A job whose pool refused it goes on to its next candidate in the
/// next wave. A chunk of one job is the serial algorithm.
///
/// Every outcome is applied before the first error is returned, so a
/// dispatch that reached a pool is never left unrecorded.
fn dispatch_chunk(
    ctx: &Ctx,
    st: &mut impl JobStore,
    chunk: &[PoolJob],
    free: &mut BTreeMap<String, u32>,
) -> Result<Vec<String>, VivariumError> {
    let mut active: Vec<Attempt> = chunk.iter().map(|job| attempt_for(ctx, job)).collect();
    let mut started = Vec::new();
    let mut failure: Option<VivariumError> = None;
    while failure.is_none() {
        let wave = plan_wave(&mut active, free);
        if wave.is_empty() {
            break;
        }
        let limit = ctx.cfg.dispatch.parallel as usize;
        let results = map_bounded(&wave, limit, |(index, pool)| {
            send_and_dispatch(ctx, &active[*index].snapshot, pool)
        });
        let mut finished = BTreeSet::new();
        for ((index, pool), sent) in wave.iter().zip(results) {
            let sent = sent.unwrap_or_else(|| {
                Sent::Failed(VivariumError::Message(
                    "job dispatch thread panicked".into(),
                ))
            });
            let snapshot = &active[*index].snapshot;
            match apply_dispatch(ctx, st, snapshot, pool, sent, free) {
                Ok(true) => {
                    started.push(snapshot.id.clone());
                    finished.insert(*index);
                }
                Ok(false) => refresh_snapshot(st, &mut active[*index]),
                Err(error) => {
                    failure.get_or_insert(error);
                    finished.insert(*index);
                }
            }
        }
        drop_finished(&mut active, &finished);
    }
    failure.map_or(Ok(started), Err)
}

/// A job paired with its ordered candidate pools.
fn attempt_for(ctx: &Ctx, snapshot: &PoolJob) -> Attempt {
    let remaining = candidate_pools(
        ctx,
        &snapshot.role,
        &snapshot.pool_candidates,
        snapshot.local_only,
        &snapshot.pool_exclude,
    );
    Attempt {
        snapshot: snapshot.clone(),
        remaining: remaining.into(),
    }
}

/// Re-read a deferred job from the store so the next wave sees what the
/// failed attempt recorded.
fn refresh_snapshot(st: &impl JobStore, attempt: &mut Attempt) {
    if let Some(updated) = st.jobs().get(&attempt.snapshot.id).cloned() {
        attempt.snapshot = updated;
    }
}

/// Remove the attempts at the finished wave indices.
fn drop_finished(active: &mut Vec<Attempt>, finished: &BTreeSet<usize>) {
    let mut position = 0;
    active.retain(|_| {
        position += 1;
        !finished.contains(&(position - 1))
    });
}

/// Pick the next pool for every active job, in order: its first remaining
/// candidate with a free slot that this wave's earlier picks have not used
/// up. Candidates passed over (or about to be tried) leave the job's list; a
/// job with none left drops out of the tick.
fn plan_wave(active: &mut [Attempt], free: &BTreeMap<String, u32>) -> Vec<(usize, String)> {
    let mut picked: BTreeMap<String, u32> = BTreeMap::new();
    let mut wave = Vec::new();
    for (index, attempt) in active.iter_mut().enumerate() {
        while let Some(pool) = attempt.remaining.pop_front() {
            let used = picked.get(&pool).copied().unwrap_or(0);
            if free.get(&pool).copied().unwrap_or(0) <= used {
                continue;
            }
            *picked.entry(pool.clone()).or_default() += 1;
            wave.push((index, pool));
            break;
        }
    }
    wave
}

pub(super) fn dispatch_order(
    job: &PoolJob,
    fix_prefix: &str,
    priorities: &BTreeMap<String, i64>,
) -> (u8, Reverse<i64>, u64, String) {
    let recovery = matches!(job.kind.as_str(), "audit" | "gate" | "promote");
    let fix = job.kind.contains("fix")
        || (!fix_prefix.is_empty()
            && (job.id.starts_with(fix_prefix)
                || job.units.iter().any(|unit| unit.starts_with(fix_prefix))
                || payload_has_fix(&job.payload, fix_prefix)));
    let class = if recovery {
        0
    } else if fix {
        1
    } else {
        2
    };
    let priority = payload_priority(&job.payload, priorities);
    (class, Reverse(priority), job.queued_at, job.id.clone())
}

fn payload_has_fix(value: &serde_json::Value, fix_prefix: &str) -> bool {
    if fix_prefix.is_empty() {
        return false;
    }
    match value {
        serde_json::Value::String(value) => value.starts_with(fix_prefix),
        serde_json::Value::Array(values) => values
            .iter()
            .any(|value| payload_has_fix(value, fix_prefix)),
        serde_json::Value::Object(values) => values.iter().any(|(key, value)| {
            matches!(
                key.as_str(),
                "id" | "need" | "need_id" | "owner_need" | "unit_id"
            ) && payload_has_fix(value, fix_prefix)
                || matches!(key.as_str(), "unit_sources" | "units")
                    && payload_has_fix(value, fix_prefix)
        }),
        _ => false,
    }
}

fn payload_priority(value: &serde_json::Value, priorities: &BTreeMap<String, i64>) -> i64 {
    match value {
        serde_json::Value::Object(values) => values
            .iter()
            .map(|(key, value)| {
                if matches!(key.as_str(), "need" | "need_id" | "owner_need")
                    && let Some(need) = value.as_str()
                {
                    return priorities.get(need).copied().unwrap_or(0);
                }
                payload_priority(value, priorities)
            })
            .max()
            .unwrap_or(0),
        serde_json::Value::Array(values) => values
            .iter()
            .map(|value| payload_priority(value, priorities))
            .max()
            .unwrap_or(0),
        _ => 0,
    }
}

/// What the concurrent half of one job dispatch learned.
enum Sent {
    /// The brief could not be written.
    Failed(VivariumError),
    /// The task could not be created.
    NoTask,
    /// The task exists (`fresh` when this call created it) and `pool dispatch`
    /// answered.
    Dispatched {
        task: String,
        fresh: bool,
        code: i32,
        text: String,
    },
}

/// The remote half of dispatching one job, with no state writes: create its
/// task when it has none, then run `pool dispatch` for it on `pool`.
fn send_and_dispatch(ctx: &Ctx, snapshot: &PoolJob, pool: &str) -> Sent {
    let (task, fresh) = match &snapshot.task {
        Some(task) => (task.clone(), false),
        None => match send_task(ctx, snapshot) {
            Ok(Some(task)) => (task, true),
            Ok(None) => return Sent::NoTask,
            Err(error) => return Sent::Failed(error),
        },
    };
    let output = ctx.vivi(&pool_command(snapshot, pool, &task), 240);
    Sent::Dispatched {
        task,
        fresh,
        code: output.code,
        text: output.text,
    }
}

/// Apply one [`Sent`] to the job store: the state writes of a job dispatch.
/// Returns `true` when the job left `pending` (dispatched, or settled by
/// recovery), `false` to try the job's next pool.
fn apply_dispatch(
    ctx: &Ctx,
    st: &mut impl JobStore,
    snapshot: &PoolJob,
    pool: &str,
    sent: Sent,
    free: &mut BTreeMap<String, u32>,
) -> Result<bool, VivariumError> {
    let (task, fresh, code, text) = match sent {
        Sent::Failed(error) => return Err(error),
        Sent::NoTask => return Ok(false),
        Sent::Dispatched {
            task,
            fresh,
            code,
            text,
        } => (task, fresh, code, text),
    };
    if fresh && let Some(job) = st.jobs_mut().get_mut(&snapshot.id) {
        job.task = Some(task.clone());
    }
    if code != 0 || !text.contains("dispatched") {
        if text.contains("no free slot") {
            free.insert(pool.to_owned(), 0);
        }
        let latest = crate::pump::collect::receipt_full(ctx, &task);
        recovery::recover_task(ctx, st, &snapshot.id, &task, &latest);
        if st
            .jobs()
            .get(&snapshot.id)
            .is_some_and(|job| job.status != JobStatus::Pending)
        {
            return Ok(true);
        }
        ctx.log(&format!(
            "job dispatch deferred {} on {pool}: {}",
            snapshot.id,
            tail(&text, 240)
        ));
        return Ok(false);
    }
    let job = st.jobs_mut().get_mut(&snapshot.id).ok_or_else(|| {
        VivariumError::Message(format!(
            "pump job {:?} disappeared during dispatch",
            snapshot.id
        ))
    })?;
    job.pool = Some(pool.to_owned());
    job.dispatched_at = Some(ctx.now());
    job.status = JobStatus::Dispatched;
    free.entry(pool.to_owned())
        .and_modify(|count| *count = count.saturating_sub(1));
    ctx.log(&format!(
        "job dispatched {} role={} pool={pool} task={task}",
        snapshot.id, snapshot.role
    ));
    Ok(true)
}

pub(super) fn send_task(ctx: &Ctx, job: &PoolJob) -> Result<Option<String>, VivariumError> {
    let dir = ctx.home.join("job-briefs");
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.txt", job.id));
    let body = format!(
        "pump-job-id: {}\npump-job-kind: {}\n{}{}\n{}",
        job.id,
        job.kind,
        job.builtin_role
            .as_deref()
            .map_or(String::new(), |role| format!("pump-builtin-role: {role}\n")),
        "",
        job.body,
    );
    let body = fit_brief(ctx, job, &dir, body)?;
    fs::write(&path, body)?;
    let subject = format!("[pump-job:{}] {}", job.id, job.subject);
    let command = format!(
        "task send --from {} --to {} --subject {} --body-file {}",
        shell_quote(&ctx.cfg.from),
        shell_quote(&job.role),
        shell_quote(&subject),
        shell_quote(&path.to_string_lossy()),
    );
    let output = ctx.vivi(&command, 120);
    if output.code != 0 {
        ctx.log(&format!(
            "job task send failed {}: {}",
            job.id,
            tail(&output.text, 240)
        ));
    }
    Ok(dispatch::parse_created_handle(&output.text))
}

/// Keep the brief under the letter cap: the full text is saved beside it, the
/// sent copy loses its middle, and one `job-brief-truncated` exception records
/// the cut so it is never silent.
fn fit_brief(ctx: &Ctx, job: &PoolJob, dir: &Path, body: String) -> Result<String, VivariumError> {
    let full = dir.join(format!("{}.full.txt", job.id));
    let Some(cut) = brief::fit(&body, &full.to_string_lossy()) else {
        return Ok(body);
    };
    fs::write(&full, &body)?;
    ctx.exception(
        "job-brief-truncated",
        &job.id,
        &format!(
            "brief was {} bytes, over the letter cap; sent {} bytes, full text at {}",
            body.len(),
            cut.len(),
            full.display()
        ),
    );
    Ok(cut)
}

pub(super) fn pool_command(job: &PoolJob, pool: &str, task: &str) -> String {
    let mut args = vec![
        "pool dispatch".to_string(),
        format!("--pool {}", shell_quote(pool)),
        shell_quote(task),
    ];
    if let Some(target) = &job.target {
        args.push(format!("--target {}", shell_quote(target)));
    }
    for extra in &job.extra_targets {
        args.push(format!("--also-target {}", shell_quote(extra)));
    }
    if let Some(from) = &job.from_ref {
        args.push(format!("--from {}", shell_quote(from)));
    }
    for extra in &job.with {
        args.push(format!("--with {}", shell_quote(extra)));
    }
    if let Some(model) = &job.model {
        args.push(format!("--model {}", shell_quote(model)));
    }
    if let Some(branch) = &job.branch {
        args.push(format!("--branch {}", shell_quote(branch)));
    }
    if job.allow_default_branch {
        args.push("--allow-default-branch".into());
    }
    append_runner_arguments(&mut args, job);
    if job.budget > 0 {
        args.push(format!("--budget {}", job.budget));
    }
    if job.read_only {
        args.push("--read-only".into());
    }
    args.push("--porcelain".into());
    args.join(" ")
}

fn append_runner_arguments(args: &mut Vec<String>, job: &PoolJob) {
    for (flag, value) in [
        ("--publish-ref", &job.publish_ref),
        ("--publish-expect", &job.publish_expect),
        ("--publish-contains", &job.publish_contains),
        ("--delete-ref", &job.delete_ref),
        ("--delete-expect", &job.delete_expect),
    ] {
        if let Some(value) = value {
            args.push(format!("{flag} {}", shell_quote(value)));
        }
    }
}

/// Expand the assigned role pool first, then explicit candidates, including
/// each pool's configured fallback chain in that order.
#[must_use]
pub fn candidate_pools(
    ctx: &Ctx,
    role_name: &str,
    candidates: &[String],
    local_only: bool,
    excluded: &[String],
) -> Vec<String> {
    let assigned = ctx
        .routing
        .lookup(role_name)
        .and_then(|role| role.pool.clone());
    let starts = if local_only {
        if candidates.is_empty() {
            assigned.into_iter().collect()
        } else {
            candidates.to_vec()
        }
    } else {
        assigned
            .into_iter()
            .chain(candidates.iter().cloned())
            .collect()
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut pools = Vec::new();
    for candidate in starts {
        let mut chain = if local_only {
            vec![candidate.clone()]
        } else {
            ctx.routing.fallback_chain(&candidate)
        };
        if chain.is_empty() {
            chain.push(candidate);
        }
        for pool in chain {
            let local = ctx
                .routing
                .pools
                .get(&pool)
                .is_some_and(|entry| entry.provider == "local");
            if (!local_only || local)
                && !excluded.iter().any(|excluded| excluded == &pool)
                && seen.insert(pool.clone())
            {
                pools.push(pool);
            }
        }
    }
    pools
}
