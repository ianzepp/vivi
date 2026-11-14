//! Pool jobs by slot, need and time, read from the `slot_runs` index.
//!
//! The index is derived from the `pool_dispatched` and `pool_collected` events
//! and written in their transaction; these methods only read it, and add the
//! display handle of the task each job belongs to.

use serde::Serialize;

use super::Mailspace;
use vivi_error::VivariumError;
use vivi_storage::storage::{SlotRun, Storage};

/// A [`SlotRun`] with the display handle of its task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SlotRunView {
    /// The task's handle, as `vivi task show` takes it.
    pub task: String,
    #[serde(flatten)]
    pub run: SlotRun,
}

fn viewed(storage: &Storage, runs: Vec<SlotRun>) -> Result<Vec<SlotRunView>, VivariumError> {
    runs.into_iter()
        .map(|run| {
            Ok(SlotRunView {
                task: storage.display_handle(&run.task_message_id)?,
                run,
            })
        })
        .collect()
}

impl Mailspace {
    /// The run for one job.
    ///
    /// # Errors
    /// Returns an error if the index cannot be read.
    pub fn slot_run(&self, job: &str) -> Result<Option<SlotRunView>, VivariumError> {
        let storage = self.storage()?;
        let runs = storage.slot_run(job)?.into_iter().collect();
        Ok(viewed(&storage, runs)?.pop())
    }

    /// Runs dispatched and not yet collected, oldest dispatch first.
    ///
    /// # Errors
    /// Returns an error if the index cannot be read.
    pub fn open_slot_runs(&self) -> Result<Vec<SlotRunView>, VivariumError> {
        let storage = self.storage()?;
        viewed(&storage, storage.open_slot_runs()?)
    }

    /// A slot's runs in one pool, newest dispatch first, at most `limit`.
    ///
    /// # Errors
    /// Returns an error if the index cannot be read.
    pub fn slot_history(
        &self,
        pool: &str,
        slot: &str,
        limit: usize,
    ) -> Result<Vec<SlotRunView>, VivariumError> {
        let storage = self.storage()?;
        viewed(&storage, storage.slot_run_history(pool, slot, limit)?)
    }

    /// A slot's runs across every configured pool, newest dispatch first, at
    /// most `limit`. The index is keyed by pool then slot, so each configured
    /// pool is asked in turn.
    ///
    /// # Errors
    /// Returns an error if the index cannot be read.
    pub fn slot_history_any_pool(
        &self,
        slot: &str,
        limit: usize,
    ) -> Result<Vec<SlotRunView>, VivariumError> {
        let storage = self.storage()?;
        let mut runs = Vec::new();
        for pool in self.config.pools.keys() {
            runs.extend(storage.slot_run_history(pool, slot, limit)?);
        }
        runs.sort_by_key(|run| std::cmp::Reverse(run.dispatched_at));
        runs.truncate(limit);
        viewed(&storage, runs)
    }

    /// A need's runs, newest dispatch first, at most `limit`.
    ///
    /// # Errors
    /// Returns an error if the index cannot be read.
    pub fn need_slot_runs(
        &self,
        need: &str,
        limit: usize,
    ) -> Result<Vec<SlotRunView>, VivariumError> {
        let storage = self.storage()?;
        viewed(&storage, storage.slot_runs_for_need(need, limit)?)
    }

    /// Runs dispatched at or after `since` (epoch seconds), newest first, at
    /// most `limit`.
    ///
    /// # Errors
    /// Returns an error if the index cannot be read.
    pub fn recent_slot_runs(
        &self,
        since: i64,
        limit: usize,
    ) -> Result<Vec<SlotRunView>, VivariumError> {
        let storage = self.storage()?;
        viewed(&storage, storage.recent_slot_runs(since, limit)?)
    }

    /// Rebuild the index from the pool events. Returns the number of rows.
    ///
    /// # Errors
    /// Returns an error if the events cannot be read or the rows written.
    pub fn rebuild_slot_runs(&self) -> Result<usize, VivariumError> {
        self.storage()?.rebuild_slot_runs()
    }
}

#[cfg(test)]
#[path = "slot_runs_test.rs"]
pub(in crate::mailspace) mod tests;
