//! Shared replay copying and internal replay payload metadata.
//!
//! [`copy_spec`] is the one entry the flow reruns and the operator requeue
//! use to copy a durable job for another execution: every execution and
//! input field travels, while runtime ownership and result state is left
//! behind. The field list itself stays in one place, on
//! [`PoolJob::rerun_spec`], so a new [`JobSpec`] field cannot survive one
//! copy path and vanish from another.
//!
//! The payload keys here are internal replay metadata, written by the
//! stampers and read back by the readers. Every reader accepts historical
//! jobs: a missing key, or a value of the wrong shape, reads as the default
//! and never fails. `input_generation` says which generation of the durable
//! inputs a job runs with; a copy preserves it, and new input replaces it
//! only by stamping a new generation explicitly. `replaces` and
//! `replacement_order` name the superseded execution and keep replacements
//! ordered: the order always rises past the execution being superseded, so
//! a copy made from an earlier substantive request still lands after the
//! later executions it supersedes. `not_before` holds a job back from
//! dispatch until an epoch second.

use serde_json::Value;

use crate::pump::jobs::{JobSpec, PoolJob};

/// The payload key holding the input generation; absent on historical jobs.
const INPUT_GENERATION: &str = "input_generation";
/// The payload key holding the superseded job's id; absent on historical jobs.
const REPLACES: &str = "replaces";
/// The payload key holding the replacement order; absent on historical jobs.
const REPLACEMENT_ORDER: &str = "replacement_order";
/// The payload key holding the earliest dispatch second; absent on
/// historical jobs.
const NOT_BEFORE: &str = "not_before";

/// Copy `job` into a fresh pending [`JobSpec`] named `new_id`: every
/// execution and input field travels, including the payload's replay
/// metadata, so a copy keeps the input generation it was built from. New
/// input replaces that generation only by an explicit
/// [`replace_input`] stamp. Runtime ownership and result fields — pool,
/// timestamps, status, outcome, applied-at, refusal counters and held
/// state — have no spec identity and are left behind.
#[must_use]
pub(crate) fn copy_spec(job: &PoolJob, new_id: impl Into<String>) -> JobSpec {
    job.rerun_spec(new_id)
}

/// The input generation of `payload`; `0` when the job predates the key.
#[must_use]
pub(crate) fn input_generation(payload: &Value) -> u64 {
    count(payload, INPUT_GENERATION)
}

/// The replacement order of `payload`; `0` when the job predates the key.
#[must_use]
pub(crate) fn replacement_order(payload: &Value) -> u64 {
    count(payload, REPLACEMENT_ORDER)
}

/// The id of the execution `payload` supersedes; `None` when the job
/// predates the key.
#[must_use]
pub(crate) fn replacement_predecessor(payload: &Value) -> Option<&str> {
    payload.get(REPLACES).and_then(Value::as_str)
}

/// The epoch second before which `payload`'s job must not dispatch; `None`
/// when the job carries no dispatch hold.
#[must_use]
pub(crate) fn dispatch_not_before(payload: &Value) -> Option<u64> {
    payload.get(NOT_BEFORE).and_then(Value::as_u64)
}

/// Stamp `generation` as the payload's input generation: new input
/// explicitly replaces the generation a copy would otherwise preserve.
pub(crate) fn replace_input(payload: &mut Value, generation: u64) {
    set_count(payload, INPUT_GENERATION, generation);
}

/// Hold `payload`'s job back from dispatch until the epoch second `at`.
pub(crate) fn stamp_dispatch_not_before(payload: &mut Value, at: u64) {
    set_count(payload, NOT_BEFORE, at);
}

/// Stamp the copy in `payload` as superseding `superseded`: it names that
/// execution as its predecessor and raises the replacement order past the
/// superseded execution's own order. A copy made from an earlier
/// substantive request rather than the latest correction therefore still
/// lands after the execution it supersedes, not after the one it copied.
pub(crate) fn stamp_replacement(payload: &mut Value, superseded: &PoolJob) {
    set_count(
        payload,
        REPLACEMENT_ORDER,
        replacement_order(&superseded.payload) + 1,
    );
    ensure_object(payload);
    if let Some(map) = payload.as_object_mut() {
        map.insert(REPLACES.into(), superseded.id.clone().into());
    }
}

/// The u64 a key reads as, `0` when absent or not a number.
fn count(payload: &Value, key: &str) -> u64 {
    payload.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn set_count(payload: &mut Value, key: &str, value: u64) {
    ensure_object(payload);
    if let Some(map) = payload.as_object_mut() {
        map.insert(key.into(), value.into());
    }
}

/// A payload that is not an object carries no metadata; stamping starts a
/// fresh one.
fn ensure_object(payload: &mut Value) {
    if !payload.is_object() {
        *payload = Value::Object(serde_json::Map::new());
    }
}

#[cfg(test)]
#[path = "replay_test.rs"]
mod tests;
