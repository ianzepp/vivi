//! Recovery tables beside the v8 schema: logical execution owners and their
//! dispatch associations.
//!
//! One logical step execution is owned by exactly one pump job at a time.
//! Owner rows keep that association durable across pump restarts: they are
//! keyed by need/run/task/node/phase plus an execution sequence, so a re-run
//! of the same phase appends a new row and history stays addressable.
//! Dispatch-start evidence and the associations accepted at dispatch (task,
//! pool, slot, pool job) live in their own tables: a pool job's identity is
//! not the pump job's identity, and the serialized
//! [`PoolJob`](crate::pump::jobs::PoolJob) snapshot carries the dispatch's
//! execution fields instead of duplicating them into columns.
//!
//! Like the flow index, the tables are created with `IF NOT EXISTS` on every
//! open and are not part of the [`State`](crate::pump::state::State) save, so
//! `save(State::default())` and a reopen never touch them. The schema version
//! is not bumped here.

#![cfg_attr(not(test), allow(dead_code))]

mod dispatch;
mod owners;

use rusqlite::Connection;

use crate::error::VivariumError;

pub(crate) use dispatch::{DispatchAccept, DispatchStart, snapshot_json};
pub(crate) use owners::ExecutionOwner;

use super::db_err;

const DDL: &str = "
CREATE TABLE IF NOT EXISTS recovery_owners (
    need TEXT NOT NULL,
    run INTEGER NOT NULL,
    task TEXT NOT NULL,
    node TEXT NOT NULL,
    phase TEXT NOT NULL,
    seq INTEGER NOT NULL,
    job TEXT,
    inputs_json TEXT NOT NULL DEFAULT '{}',
    transient_json TEXT NOT NULL DEFAULT '{}',
    allowance INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (need, run, task, node, phase, seq)
);
CREATE INDEX IF NOT EXISTS recovery_owners_job ON recovery_owners (job);
CREATE TABLE IF NOT EXISTS recovery_dispatch_starts (
    job TEXT PRIMARY KEY,
    at INTEGER NOT NULL,
    evidence_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS recovery_dispatch_accepts (
    job TEXT NOT NULL PRIMARY KEY,
    task TEXT,
    pool TEXT,
    slot TEXT,
    pool_job TEXT,
    snapshot_json TEXT
);
";

/// Create the recovery tables when they are missing.
pub(super) fn ensure_tables(conn: &Connection) -> Result<(), VivariumError> {
    conn.execute_batch(DDL)
        .map_err(db_err("create recovery tables"))
}

#[cfg(test)]
#[path = "recovery_test.rs"]
mod tests;
