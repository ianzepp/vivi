//! Ranking of flow work: repair kind, then CTO steps, then need priority, then age.
//!
//! Pure functions, no I/O. A flow step job's payload carries `"need"`,
//! `"kind"` (the run's flow kind) and `"class"` (the node class name); the
//! tick writes them and this module reads them.

use std::cmp::Reverse;
use std::collections::BTreeMap;

use crate::pump::jobs::{JobStatus, PoolJob};

/// Sort key shared with `dispatcher::dispatch_order`.
pub type RankKey = (u8, Reverse<i64>, u64, String);

const KIND_REPAIR: &str = "repair";
const CLASS_CTO: &str = "cto";
const CLASS_MERGE_MAIN: &str = "merge-main";

fn payload_str<'a>(job: &'a PoolJob, key: &str) -> Option<&'a str> {
    job.payload.get(key).and_then(serde_json::Value::as_str)
}

/// Rank of a flow step job; `None` for an old-pipeline job (no payload kind).
#[must_use]
pub fn flow_key(job: &PoolJob, priorities: &BTreeMap<String, i64>) -> Option<RankKey> {
    let kind = payload_str(job, "kind")?;
    let class = if kind == KIND_REPAIR {
        0
    } else if payload_str(job, "class") == Some(CLASS_CTO) {
        1
    } else {
        2
    };
    let priority = payload_str(job, "need")
        .and_then(|need| priorities.get(need))
        .copied()
        .unwrap_or(0);
    Some((class, Reverse(priority), job.queued_at, job.id.clone()))
}

/// Rank of an intake candidate: repair first, then priority, then oldest, then handle.
#[must_use]
pub fn need_key(repair: bool, priority: i64, age: u64, need: &str) -> RankKey {
    (u8::from(!repair), Reverse(priority), age, need.to_owned())
}

fn is_merge_main(job: &PoolJob) -> bool {
    payload_str(job, "class") == Some(CLASS_MERGE_MAIN)
}

/// Keep `pending` in order but at most one merge-main per target in flight:
/// a merge-main job waits while its target has a dispatched merge-main in
/// `jobs` or an earlier kept one in `pending`.
#[must_use]
pub fn single_flight(pending: Vec<PoolJob>, jobs: &BTreeMap<String, PoolJob>) -> Vec<PoolJob> {
    let mut busy: Vec<Option<String>> = jobs
        .values()
        .filter(|job| job.status == JobStatus::Dispatched && is_merge_main(job))
        .map(|job| job.target.clone())
        .collect();
    let mut kept = Vec::with_capacity(pending.len());
    for job in pending {
        if is_merge_main(&job) {
            if busy.contains(&job.target) {
                continue;
            }
            busy.push(job.target.clone());
        }
        kept.push(job);
    }
    kept
}

#[cfg(test)]
#[path = "rank_test.rs"]
mod tests;
