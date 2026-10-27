//! The execution-aware reader of one step execution's pool receipt.
//!
//! A step execution's receipt is not "the latest letter on the task": pool
//! slots are reused, replacements re-dispatch the same task, and one task
//! thread can outlive several executions. This module is the shared reader
//! that selects the dispatch and the result of exactly one execution from
//! the full typed pool notes of the task thread — the `[pool]` records of
//! `pool_dispatched` and `pool_collected` events, parsed as
//! [`DispatchRecord`](crate::pool::record::DispatchRecord) — never a
//! `task show --latest` glance at whatever answered last.
//!
//! Selection anchors on the evidence the pump saved for its own job: the
//! association a pool accepted (task, pool, slot, pool-generated job id)
//! and the dispatch start it recorded. With a recorded pool job the thread
//! must carry exactly that dispatch, and its pool and slot must match the
//! saved association. Without a recorded acceptance, the saved start time,
//! the prior dispatch identities already claimed on the thread and the pump
//! brief note naming this job must together single out exactly one new
//! dispatch; anything else stays ambiguous. A pending replacement without
//! its own start evidence never adopts an older execution's dispatch.
//!
//! The reader decides for one execution's view. Deciding between competing
//! executions is the collectors' concern, not this module's.

use crate::error::VivariumError;
use crate::mailspace::PoolEventRow;
use crate::pool::record::DispatchRecord;
use crate::pump::store::PumpStore;

/// The dispatch evidence the pump saved for one pump job.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SavedDispatch {
    /// When the pump recorded this job's dispatch starting, if it did.
    pub started_at: Option<u64>,
    /// The association a pool accepted for this job, if it was recorded.
    pub accepted: Option<AcceptedDispatch>,
}

/// The association a pool accepted for one pump job: the mailspace task,
/// the pool, the slot, and the pool's own generated job id — never the
/// pump job id.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AcceptedDispatch {
    pub task: Option<String>,
    pub pool: Option<String>,
    pub slot: Option<String>,
    pub pool_job: Option<String>,
}

/// One execution's identity and the typed evidence gathered from its task.
#[derive(Debug, Clone, Copy)]
pub struct ExecutionQuery<'a> {
    /// The pump job id of the execution; never a pool job id.
    pub job: &'a str,
    /// The task the execution runs on.
    pub task: &'a str,
    /// Typed pool notes read from the mailspace, oldest first. Notes of
    /// other tasks are ignored.
    pub events: &'a [PoolEventRow],
    /// Whether the task thread carries a pump brief note naming `job`.
    pub briefed: bool,
    /// Pool job ids already claimed by earlier executions of this task:
    /// never candidates for this one, even after its start time.
    pub prior_pool_jobs: &'a [String],
}

/// The dispatch of one execution as the task thread records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedDispatch {
    /// The mailspace task the dispatch ran on.
    pub task: String,
    /// The pool whose slot ran the job.
    pub pool: String,
    /// The slot that ran the job.
    pub slot: String,
    /// The pool's own job id for this dispatch; never the pump job id.
    pub pool_job: String,
    /// The full typed dispatch record.
    pub record: DispatchRecord,
}

/// The terminal result collected for one execution's dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedResult {
    /// The dispatch the result closes.
    pub dispatch: SelectedDispatch,
    /// `finished`, `timeout` or `lost`, from the collected note.
    pub outcome: String,
    /// The collected `[pool]` JSON note, verbatim.
    pub note: String,
}

/// What the task thread records for one execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionReceipt {
    /// The store holds no dispatch evidence for this pump job: it never
    /// started one. No thread dispatch may be adopted on this basis alone.
    Absent,
    /// The execution began (or holds a saved association) but no dispatch
    /// on the task is attributable to it. A pending replacement without its
    /// own start evidence lands here instead of adopting an old dispatch.
    Unstarted,
    /// Exactly one dispatch is attributable and no result is recorded yet.
    Dispatched(SelectedDispatch),
    /// The attributable dispatch collected a terminal result.
    Terminal(SelectedResult),
    /// The saved records and the task thread disagree, or more than one
    /// dispatch could belong to this execution; nothing was chosen.
    Ambiguous(String),
}

/// Load the dispatch evidence the pump saved for one pump job.
///
/// # Errors
/// Returns [`VivariumError`] when the saved evidence cannot be read.
pub fn load(store: &PumpStore, job: &str) -> Result<SavedDispatch, VivariumError> {
    let started_at = store.dispatch_start(job)?.map(|start| start.at);
    let accepted = store.dispatch_accept(job)?.map(|accept| AcceptedDispatch {
        task: accept.task,
        pool: accept.pool,
        slot: accept.slot,
        pool_job: accept.pool_job,
    });
    Ok(SavedDispatch {
        started_at,
        accepted,
    })
}

/// Read one execution's receipt: the saved evidence of its pump job
/// selected against the typed pool notes of its task.
///
/// # Errors
/// Returns [`VivariumError`] when the saved evidence cannot be read.
pub fn receipt(
    store: &PumpStore,
    query: ExecutionQuery<'_>,
) -> Result<ExecutionReceipt, VivariumError> {
    Ok(select(&load(store, query.job)?, query))
}

/// Select one execution's receipt from already-loaded evidence.
#[must_use]
pub fn select(saved: &SavedDispatch, query: ExecutionQuery<'_>) -> ExecutionReceipt {
    if saved.started_at.is_none() && saved.accepted.is_none() {
        return ExecutionReceipt::Absent;
    }
    let thread = Thread::parse(query.task, query.events);
    let accepted = saved.accepted.as_ref();
    let saved_task = accepted.and_then(|a| a.task.as_deref());
    if saved_task.is_some_and(|saved| saved != query.task) {
        return ExecutionReceipt::Ambiguous(format!(
            "saved dispatch names task {} but this execution runs on {task}",
            saved_task.unwrap_or_default(),
            task = query.task,
        ));
    }
    let saved_pool = accepted
        .and_then(|a| a.pool.as_deref())
        .filter(|pool| !pool.is_empty());
    let saved_slot = accepted
        .and_then(|a| a.slot.as_deref())
        .filter(|slot| !slot.is_empty());
    if let Some(pool_job) = accepted.and_then(|a| a.pool_job.as_deref()) {
        return thread.by_saved(pool_job, saved_pool, saved_slot);
    }
    let Some(started_at) = saved.started_at else {
        return ExecutionReceipt::Unstarted;
    };
    let candidates = thread.after(started_at, saved_pool, saved_slot, query.prior_pool_jobs);
    match candidates.len() {
        0 => ExecutionReceipt::Unstarted,
        1 if query.briefed => thread.selected(candidates[0]),
        1 => ExecutionReceipt::Ambiguous(format!(
            "dispatch start evidence names job {} but the task carries no pump brief for it",
            query.job
        )),
        _ => ExecutionReceipt::Ambiguous(format!(
            "{} dispatches follow job {}'s dispatch start on {}; none can be preferred",
            candidates.len(),
            query.job,
            query.task
        )),
    }
}

/// The typed pool notes of one task thread, oldest first.
struct Thread {
    task: String,
    dispatches: Vec<ThreadDispatch>,
}

/// One dispatch on the thread and the result note that collected it, if any.
struct ThreadDispatch {
    record: DispatchRecord,
    result: Option<ThreadResult>,
}

/// A collected note parsed to the fields the reader verifies.
struct ThreadResult {
    outcome: String,
    pool: Option<String>,
    slot: Option<String>,
    note: String,
}

impl Thread {
    /// Parse the typed pool notes of `task`, oldest first; notes of other
    /// tasks are ignored. A note that is not the record its event word
    /// promises is not typed evidence and is skipped.
    fn parse(task: &str, events: &[PoolEventRow]) -> Self {
        let mut thread = Self {
            task: task.to_owned(),
            dispatches: Vec::new(),
        };
        for event in events.iter().filter(|event| event.handle == task) {
            match event.event_type.as_str() {
                "pool_dispatched" => thread.dispatched(&event.note),
                "pool_collected" => thread.collected(&event.note),
                _ => {}
            }
        }
        thread
    }

    fn dispatched(&mut self, note: &str) {
        let Ok(record) = serde_json::from_str::<DispatchRecord>(note) else {
            return;
        };
        self.dispatches.push(ThreadDispatch {
            record,
            result: None,
        });
    }

    /// A collection closes the newest open dispatch of the same pool job.
    fn collected(&mut self, note: &str) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(note) else {
            return;
        };
        let job = field(&value, "job");
        let Some(job) = job.as_deref() else {
            return;
        };
        let Some(index) = self
            .dispatches
            .iter()
            .rposition(|d| d.result.is_none() && d.record.job == job)
        else {
            return;
        };
        self.dispatches[index].result = Some(ThreadResult {
            outcome: field(&value, "outcome").unwrap_or_else(|| "unknown".to_owned()),
            pool: field(&value, "pool"),
            slot: field(&value, "slot"),
            note: note.to_owned(),
        });
    }

    /// Select by the saved association: the thread must carry exactly the
    /// pool job the acceptance named, and its provenance must fit.
    fn by_saved(&self, pool_job: &str, pool: Option<&str>, slot: Option<&str>) -> ExecutionReceipt {
        let found: Vec<usize> = self
            .dispatches
            .iter()
            .enumerate()
            .filter(|(_, d)| d.record.job == pool_job)
            .map(|(index, _)| index)
            .collect();
        match found.as_slice() {
            [] => ExecutionReceipt::Ambiguous(format!(
                "saved dispatch names pool job {pool_job} but the task thread records no such dispatch"
            )),
            [index] => provenance(&self.dispatches[*index].record, pool, slot)
                .map_or_else(|| self.selected(*index), ExecutionReceipt::Ambiguous),
            _ => ExecutionReceipt::Ambiguous(format!(
                "the task thread records {} dispatches of pool job {pool_job}",
                found.len()
            )),
        }
    }

    /// The dispatches that could belong to this execution: at or after its
    /// start time, not a prior identity, and fitting the saved pool/slot.
    fn after(
        &self,
        started_at: u64,
        pool: Option<&str>,
        slot: Option<&str>,
        prior: &[String],
    ) -> Vec<usize> {
        self.dispatches
            .iter()
            .enumerate()
            .filter(|(_, d)| d.record.at >= started_at)
            .filter(|(_, d)| !prior.contains(&d.record.job))
            .filter(|(_, d)| provenance(&d.record, pool, slot).is_none())
            .map(|(index, _)| index)
            .collect()
    }

    /// Turn one thread dispatch into the execution's receipt, with the
    /// result note it was collected by, when it was.
    fn selected(&self, index: usize) -> ExecutionReceipt {
        let found = &self.dispatches[index];
        let record = &found.record;
        let dispatch = SelectedDispatch {
            task: self.task.clone(),
            pool: record.pool.clone(),
            slot: record.slot.clone(),
            pool_job: record.job.clone(),
            record: record.clone(),
        };
        let Some(result) = &found.result else {
            return ExecutionReceipt::Dispatched(dispatch);
        };
        if provenance(record, result.pool.as_deref(), result.slot.as_deref()).is_some() {
            return ExecutionReceipt::Ambiguous(format!(
                "the collected result of pool job {} disagrees with its dispatch record",
                record.job
            ));
        }
        ExecutionReceipt::Terminal(SelectedResult {
            dispatch,
            outcome: result.outcome.clone(),
            note: result.note.clone(),
        })
    }
}

/// Why `pool` or `slot` disagree with the dispatch record, or `None` when
/// the saved values fit. An unset saved value verifies nothing.
fn provenance(record: &DispatchRecord, pool: Option<&str>, slot: Option<&str>) -> Option<String> {
    if let Some(saved) = pool
        && record.pool != saved
    {
        return Some(format!(
            "saved dispatch names pool {saved} but pool job {} recorded {}",
            record.job, record.pool
        ));
    }
    if let Some(saved) = slot
        && record.slot != saved
    {
        return Some(format!(
            "saved dispatch names slot {saved} but pool job {} recorded {}",
            record.job, record.slot
        ));
    }
    None
}

/// One string field of a collected note; absent and empty read as none.
fn field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
#[path = "execution_receipt_test.rs"]
mod tests;
