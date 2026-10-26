//! The cross-run inventory of completed slices over the retained runs of
//! one need.
//!
//! Source guards and the planner, slicer and rework briefs read this one
//! inventory instead of searching the runs themselves. Entries are
//! run-qualified provenance, not proof of published code: a slice completed
//! in an older run lives, if anywhere, in an archived prior branch, so its
//! completion is a retained claim — never evidence that the code is in the
//! active head, and a `no-defect` completion is never evidence of published
//! code at all. The reader infers nothing about whether an archive is
//! fetchable and reconstructs no branch; callers decide what a claim from
//! run N means for the branch they are looking at.

use crate::error::VivariumError;
use crate::pump::flow::fanout::{VERDICT_MERGED, VERDICT_NO_DEFECT, is_slice_row};
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::store::PumpStore;

/// One completed slice, qualified by the run that retains its step row.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CompletedSlice {
    /// The run whose step row records the completion.
    pub run: u32,
    /// The slice node (`slice:<id>`).
    pub node: String,
    /// The task handle of the slice's step.
    pub task: String,
    /// The completion verdict: `merged` or `no-defect`.
    pub verdict: String,
}

/// The completed slices across every retained run of one need, ordered by
/// run, then node, then task.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SliceInventory {
    /// The completed slices, in deterministic order.
    pub entries: Vec<CompletedSlice>,
}

impl SliceInventory {
    /// How many completions merged their work onto a need branch.
    #[must_use]
    pub fn merged(&self) -> usize {
        self.count_of(VERDICT_MERGED)
    }

    /// How many completions closed `no-defect` because a hand found nothing
    /// to change. This count is never evidence of published code.
    #[must_use]
    pub fn no_defect(&self) -> usize {
        self.count_of(VERDICT_NO_DEFECT)
    }

    /// Entries from runs older than `run`: claims retained from archived
    /// priors, not proof that their code is in the active head.
    #[must_use]
    pub fn historical(&self, run: u32) -> impl Iterator<Item = &CompletedSlice> {
        self.entries.iter().filter(move |entry| entry.run < run)
    }

    /// Entries completed in `run` itself.
    #[must_use]
    pub fn in_run(&self, run: u32) -> impl Iterator<Item = &CompletedSlice> {
        self.entries.iter().filter(move |entry| entry.run == run)
    }

    fn count_of(&self, verdict: &str) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.verdict == verdict)
            .count()
    }
}

/// The completed slices across every retained run of `need`, oldest run
/// first. A slice counts once its step row is done with a completion verdict
/// (`merged` or `no-defect`); failed and exhausted slices, unfinished rows
/// and non-slice rows never enter. Completions from superseded runs stay in:
/// they are the archived priors' retained claims.
///
/// # Errors
/// Returns [`VivariumError`] when the run or step rows cannot be read.
pub fn inventory(store: &PumpStore, need: &str) -> Result<SliceInventory, VivariumError> {
    let mut entries = Vec::new();
    for run in store.runs_of(need)? {
        for step in store.steps_of(need, run.run)? {
            if is_slice_row(&step)
                && step.state == StepState::Done
                && let Some(verdict) = completion_verdict(&step).map(str::to_string)
            {
                entries.push(CompletedSlice {
                    run: run.run,
                    node: step.node,
                    task: step.task,
                    verdict: verdict.to_string(),
                });
            }
        }
    }
    entries.sort();
    Ok(SliceInventory { entries })
}

/// The completion verdict of a done slice row, or `None` for any other
/// verdict: a `replaced` slice counts through its replacements instead.
fn completion_verdict(step: &StepRow) -> Option<&str> {
    let verdict = step.verdict.as_deref()?;
    (verdict == VERDICT_MERGED || verdict == VERDICT_NO_DEFECT).then_some(verdict)
}

#[cfg(test)]
#[path = "history_test.rs"]
mod tests;
