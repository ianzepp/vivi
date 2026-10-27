//! Durable dispatch evidence for one job dispatch attempt.
//!
//! Three moments are covered. Before a pool is asked, the attempt's start is
//! recorded: the task, the pump job and brief identity, the candidate pool,
//! and the dispatch identities already on the task thread — so a lost
//! response still anchors receipt selection. When the pool's `dispatched`
//! porcelain line is parseable, the association it names (task, pool, slot,
//! pool job) is persisted before the job map is saved, closing the crash
//! window between a pool accepting work and the pump recording it. When the
//! response is uncertain, the saved evidence and the task's typed pool notes
//! decide through [`execution_receipt`] what the attempt produced; an
//! unresolved ownership holds the job instead of re-dispatching it.

use crate::error::VivariumError;
use crate::letter::records::NoteWord;
use crate::mailspace::{Mailspace, PoolEventRow};
use crate::pool::record::DispatchRecord;
use crate::pump::host::{Ctx, shell_quote, shell_split};
use crate::pump::jobs::execution_receipt::{self, ExecutionReceipt, SelectedDispatch};
use crate::pump::jobs::recovery;
use crate::pump::jobs::{JobOutcome, JobStatus, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};

/// What an uncertain dispatch response resolved to.
pub(super) enum Resolved {
    /// The attempt's own dispatch (and any collected result) was found and
    /// adopted into the job.
    Adopted,
    /// Nothing on the task belongs to this attempt: offer the next pool.
    Unstarted,
    /// Ownership cannot be resolved; the job must hold, not re-dispatch.
    Ambiguous(String),
}

/// The association a pool accepted, parsed from its `dispatched` line.
pub(super) struct Accepted {
    pub task: String,
    pub pool: String,
    pub slot: String,
    pub pool_job: String,
}

/// The first `dispatched TASK POOL SLOT JOB ...` porcelain line of `output`,
/// its fields tokenized shell-aware (values may be quoted). `None` when no
/// line parses: the response says nothing definite about an acceptance.
pub(super) fn accepted_line(output: &str) -> Option<Accepted> {
    for line in output.lines() {
        let words = shell_split(line);
        if words.first().map(String::as_str) == Some("dispatched") && words.len() >= 5 {
            return Some(Accepted {
                task: words[1].clone(),
                pool: words[2].clone(),
                slot: words[3].clone(),
                pool_job: words[4].clone(),
            });
        }
    }
    None
}

/// Open the pump store the dispatch evidence lives in. A home without
/// `pump.db` has no durable store yet — the pump run creates it before it
/// ever dispatches — so there is nothing to write into or read from.
fn open_store(ctx: &Ctx) -> Result<Option<PumpStore>, VivariumError> {
    let path = ctx.home.join(DB_FILENAME);
    if !path.is_file() {
        return Ok(None);
    }
    PumpStore::open(&path).map(Some)
}

/// Record the start of one dispatch attempt before the pool dispatch runs:
/// first writer wins, so a retried attempt keeps the original start time the
/// receipt selection anchors on.
///
/// # Errors
/// Returns [`VivariumError`] when the evidence cannot be made durable; the
/// attempt must not run without it.
pub(super) fn record_start(
    ctx: &Ctx,
    job: &PoolJob,
    pool: &str,
    task: &str,
) -> Result<(), VivariumError> {
    let Some(store) = open_store(ctx)? else {
        return Ok(());
    };
    let evidence = serde_json::json!({
        "job": job.id,
        "kind": job.kind,
        "builtin_role": job.builtin_role,
        "pool": pool,
        "task": task,
        "prior_pool_jobs": prior_pool_jobs(ctx, task),
    });
    store.save_dispatch_start(&job.id, ctx.now(), &evidence.to_string())
}

/// Persist the association a pool accepted, before the job map save it must
/// outlive.
///
/// # Errors
/// Returns [`VivariumError`] when the association cannot be made durable.
pub(super) fn persist_accept(
    ctx: &Ctx,
    job: &PoolJob,
    task: &str,
    pool: &str,
    slot: &str,
    pool_job: &str,
) -> Result<(), VivariumError> {
    let Some(store) = open_store(ctx)? else {
        return Ok(());
    };
    let snapshot = serde_json::to_string(job)
        .map_err(|error| VivariumError::Message(format!("job {} evidence: {error}", job.id)))?;
    store.save_dispatch_accept(
        &job.id,
        Some(task),
        Some(pool),
        Some(slot),
        Some(pool_job),
        Some(&snapshot),
    )
}

/// Decide an uncertain dispatch response (no parseable `dispatched` line)
/// from the saved evidence and the task's typed pool notes. Adopts the
/// attempt's own dispatch or collected result; anything else defers, and an
/// unresolved ownership holds the job.
///
/// # Errors
/// Returns [`VivariumError`] when the saved evidence cannot be read or an
/// adopted association cannot be persisted.
pub(super) fn resolve_uncertain(
    ctx: &Ctx,
    st: &mut impl JobStore,
    snapshot: &PoolJob,
    task: &str,
) -> Result<Resolved, VivariumError> {
    let Some(store) = open_store(ctx)? else {
        return Ok(Resolved::Unstarted);
    };
    let saved = execution_receipt::load(&store, &snapshot.id)?;
    if saved.started_at.is_none() && saved.accepted.is_none() {
        return Ok(Resolved::Unstarted);
    }
    let prior = prior_identities(&store, &snapshot.id)?;
    let events = typed_events(ctx);
    let briefed = brief_present(ctx, task, &snapshot.id);
    let query = execution_receipt::ExecutionQuery {
        job: &snapshot.id,
        task,
        events: &events,
        briefed,
        prior_pool_jobs: &prior,
    };
    finish(ctx, st, snapshot, &execution_receipt::select(&saved, query))
}

/// Turn a selected receipt into the resolution: adopt what belongs to the
/// attempt, hold on an unresolved ownership, defer on the rest.
fn finish(
    ctx: &Ctx,
    st: &mut impl JobStore,
    snapshot: &PoolJob,
    receipt: &ExecutionReceipt,
) -> Result<Resolved, VivariumError> {
    match receipt {
        ExecutionReceipt::Absent | ExecutionReceipt::Unstarted => Ok(Resolved::Unstarted),
        ExecutionReceipt::Ambiguous(reason) => Ok(Resolved::Ambiguous(reason.clone())),
        ExecutionReceipt::Dispatched(dispatch) => {
            adopt(ctx, st, snapshot, dispatch, None)?;
            Ok(Resolved::Adopted)
        }
        ExecutionReceipt::Terminal(result) => {
            let word = result.outcome.as_str();
            let note = result.note.as_str();
            adopt(ctx, st, snapshot, &result.dispatch, Some((word, note)))?;
            Ok(Resolved::Adopted)
        }
    }
}

/// Adopt one attributable dispatch into the pump job: the association is
/// persisted first, then the job map moves. A collected terminal result
/// settles the job with its outcome.
fn adopt(
    ctx: &Ctx,
    st: &mut impl JobStore,
    snapshot: &PoolJob,
    dispatch: &SelectedDispatch,
    terminal: Option<(&str, &str)>,
) -> Result<(), VivariumError> {
    persist_accept(
        ctx,
        snapshot,
        &dispatch.task,
        &dispatch.pool,
        &dispatch.slot,
        &dispatch.pool_job,
    )?;
    let Some(job) = st.jobs_mut().get_mut(&snapshot.id) else {
        ctx.log(&format!(
            "pump job {} gone while adopting its dispatch",
            snapshot.id
        ));
        return Ok(());
    };
    if job.task.is_none() {
        job.task = Some(dispatch.task.clone());
    }
    job.pool = Some(dispatch.pool.clone());
    job.dispatched_at.get_or_insert_with(|| ctx.now());
    let (status, outcome) = match terminal {
        Some((word, note)) => match terminal_status(word) {
            None => (JobStatus::Dispatched, None),
            Some(status) => (
                status,
                Some(JobOutcome {
                    state: word.to_owned(),
                    detail: note_detail(note, word),
                    receipt: note.to_owned(),
                    at: ctx.now(),
                }),
            ),
        },
        None => (JobStatus::Dispatched, None),
    };
    job.status = status;
    job.outcome = outcome;
    ctx.log(&format!(
        "job dispatch adopted {} pool={} slot={} pool_job={} task={}",
        snapshot.id, dispatch.pool, dispatch.slot, dispatch.pool_job, dispatch.task
    ));
    Ok(())
}

/// The terminal job status of a collected outcome word; anything else keeps
/// the dispatch open.
fn terminal_status(word: &str) -> Option<JobStatus> {
    match word {
        "finished" => Some(JobStatus::Finished),
        "timeout" => Some(JobStatus::Timeout),
        "lost" => Some(JobStatus::Lost),
        _ => None,
    }
}

/// The short detail of a collected note: its `push`, `result` or `detail`
/// field when it carries one, else the outcome word.
fn note_detail(note: &str, word: &str) -> String {
    serde_json::from_str::<serde_json::Value>(note)
        .ok()
        .and_then(|value| {
            ["push", "result", "detail"].iter().find_map(|key| {
                value
                    .get(*key)
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
        })
        .unwrap_or_else(|| word.to_owned())
}

/// Whether the task thread carries a pump brief note naming `job`.
pub(super) fn brief_present(ctx: &Ctx, task: &str, job: &str) -> bool {
    let thread = ctx.vivi(
        &format!("mail thread {} --limit 1000 --json", shell_quote(task)),
        60,
    );
    thread.code == 0
        && recovery::bodies_of(&thread.text).iter().any(|body| {
            crate::mailspace::pump_note::note_of(body).is_some_and(|note| {
                note.pump.note == NoteWord::Brief
                    && note.job.is_some_and(|front| front.id == job)
            })
        })
}

/// The pool job ids already recorded on the task thread before this attempt:
/// saved with the start evidence so selection never adopts them.
fn prior_pool_jobs(ctx: &Ctx, task: &str) -> Vec<String> {
    typed_events(ctx)
        .iter()
        .filter(|event| event.handle == task && event.event_type == "pool_dispatched")
        .filter_map(|event| serde_json::from_str::<DispatchRecord>(&event.note).ok())
        .map(|record| record.job)
        .collect()
}

/// The saved dispatch identities of the job's own start evidence.
fn prior_identities(store: &PumpStore, job: &str) -> Result<Vec<String>, VivariumError> {
    let Some(evidence) = store.dispatch_start(job)?.map(|start| start.evidence_json) else {
        return Ok(Vec::new());
    };
    let value = serde_json::from_str::<serde_json::Value>(&evidence).unwrap_or_default();
    Ok(value
        .get("prior_pool_jobs")
        .and_then(serde_json::Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default())
}

/// Every typed pool note in the mailspace, oldest first. A mailspace that
/// cannot be read contributes none: selection then has no thread to adopt
/// from and defers.
fn typed_events(ctx: &Ctx) -> Vec<PoolEventRow> {
    let read = Mailspace::discover(Some(&ctx.project)).and_then(|space| space.pool_events());
    match read {
        Ok(events) => events,
        Err(error) => {
            ctx.log(&format!("dispatch receipt selection: {error}"));
            Vec::new()
        }
    }
}
