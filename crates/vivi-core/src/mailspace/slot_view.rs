//! The declared slots, each with the job it is running.
//!
//! A slot is busy while its latest dispatch has no collect after it. That is
//! read from the `slot_runs` index, never from the host: the view needs no
//! machine to be reachable.

use serde::Serialize;

use super::Mailspace;
use super::slot_runs::SlotRunView;
use vivi_error::VivariumError;

/// Whether a slot holds an open job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SlotState {
    Busy,
    Free,
}

impl SlotState {
    /// `busy` or `free`.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Busy => "busy",
            Self::Free => "free",
        }
    }
}

/// One slot and its open job, if any.
#[derive(Debug, Clone, Serialize)]
pub struct SlotRow {
    pub slot: String,
    /// `owner/repo` the slot is bound to; empty for a slot the config does
    /// not declare.
    pub repository: String,
    /// The host the slot lives on; empty for an undeclared slot.
    pub host: String,
    /// The pools that may use the slot.
    pub pools: Vec<String>,
    /// Whether `[[slot]]` declares it. A job can run in a slot that has since
    /// been removed from the config, or in a pool that counts its slots.
    pub declared: bool,
    pub state: SlotState,
    /// The open job: the newest dispatch not yet collected.
    pub run: Option<SlotRunView>,
}

impl Mailspace {
    /// Every declared slot in config order, then the slots that have an open
    /// job but no declaration, oldest dispatch first.
    ///
    /// # Errors
    /// Returns an error if the index cannot be read.
    pub fn slot_rows(&self) -> Result<Vec<SlotRow>, VivariumError> {
        let mut open = self.open_slot_runs()?;
        let mut rows = Vec::new();
        for slot in &self.config.slots {
            let run = take_newest(&mut open, &slot.name);
            let pools = if slot.pools.is_empty() {
                self.config.pools.keys().cloned().collect()
            } else {
                slot.pools.clone()
            };
            rows.push(row(
                &slot.name,
                &slot.repository,
                &slot.host,
                pools,
                true,
                run,
            ));
        }
        while let Some(name) = open.first().map(|run| run.run.slot.clone()) {
            let run = take_newest(&mut open, &name);
            let pools = run.iter().map(|run| run.run.pool.clone()).collect();
            rows.push(row(&name, "", "", pools, false, run));
        }
        Ok(rows)
    }

    /// One slot by name, declared or running a job.
    ///
    /// # Errors
    /// Returns an error if the index cannot be read.
    pub fn slot_row(&self, name: &str) -> Result<Option<SlotRow>, VivariumError> {
        Ok(self.slot_rows()?.into_iter().find(|row| row.slot == name))
    }
}

/// Remove and return the newest run in `slot`; any older open run in the same
/// slot is a dispatch whose collect was never recorded, and is dropped.
fn take_newest(open: &mut Vec<SlotRunView>, slot: &str) -> Option<SlotRunView> {
    let mut mine: Vec<SlotRunView> = Vec::new();
    open.retain(|run| {
        let is_mine = run.run.slot == slot;
        if is_mine {
            mine.push(run.clone());
        }
        !is_mine
    });
    mine.into_iter().max_by_key(|run| run.run.dispatched_at)
}

fn row(
    slot: &str,
    repository: &str,
    host: &str,
    pools: Vec<String>,
    declared: bool,
    run: Option<SlotRunView>,
) -> SlotRow {
    SlotRow {
        slot: slot.into(),
        repository: repository.into(),
        host: host.into(),
        pools,
        declared,
        state: if run.is_some() {
            SlotState::Busy
        } else {
            SlotState::Free
        },
        run,
    }
}

#[cfg(test)]
#[path = "slot_view_test.rs"]
mod tests;
