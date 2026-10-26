//! Retained work history of a recorded need branch.
//!
//! The completed slice outcomes a later run can stand on, read from the
//! durable run and step rows only — no Git. A slice outcome is kept when its
//! step is a done slice row closed `merged` or `no-defect`, in any carried
//! run through the run that owns the need branch row — including runs that
//! were superseded, because their merged work is still on the branch.
//! Unfinished steps and `failed`, `replaced` or `superseded` slice outcomes
//! stay out. Runs at or before the newest archival boundary — the newest
//! `vivi/need-prior/<need>-r<run>` name in the row's priors — moved to that
//! prior branch and are excluded from the active contents.

use crate::error::VivariumError;
use crate::pump::flow::fanout::{VERDICT_MERGED, VERDICT_NO_DEFECT, is_slice_row};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::need_branch::prior;
use crate::pump::store::{NeedBranchRow, PumpStore};

/// One completed slice outcome: its source run, node and task, and the
/// verdict it closed with.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct RetainedSlice {
    /// The run the slice was cut in.
    pub run: u32,
    /// The slice node id (`slice:<id>`).
    pub node: String,
    /// The slice task handle.
    pub task: String,
    /// The closing verdict: `merged` or `no-defect`.
    pub verdict: String,
}

/// The retained work history of one recorded need branch.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Retained {
    /// The target the branch lives on.
    pub target: String,
    /// The branch name.
    pub branch: String,
    /// The original base recorded when the owning run was admitted.
    pub base: Option<String>,
    /// The run that owns the need branch row.
    pub run: u32,
    /// The newest archival boundary: slices at or before this run live on a
    /// prior branch. `None` when nothing was archived.
    pub archived_through: Option<u32>,
    /// Every completed merged slice through the owning run, identity order.
    pub merged: Vec<RetainedSlice>,
    /// Completed no-defect slices through the owning run, identity order:
    /// the hand found nothing to change, so nothing merged.
    pub no_defect: Vec<RetainedSlice>,
}

impl Retained {
    /// The slices of `all` after the archival boundary: the active contents.
    fn active<'a>(
        &'a self,
        all: &'a [RetainedSlice],
    ) -> impl Iterator<Item = &'a RetainedSlice> + 'a {
        let floor = self.archived_through.unwrap_or(0);
        all.iter().filter(move |slice| slice.run > floor)
    }

    /// The merged slices after the archival boundary.
    #[allow(dead_code)]
    #[must_use]
    pub(crate) fn active_merged(&self) -> impl Iterator<Item = &RetainedSlice> {
        self.active(&self.merged)
    }

    /// The no-defect slices after the archival boundary.
    #[allow(dead_code)]
    #[must_use]
    pub(crate) fn active_no_defect(&self) -> impl Iterator<Item = &RetainedSlice> {
        self.active(&self.no_defect)
    }
}

/// The retained history of `row`, from the durable run and step rows.
///
/// # Errors
/// Returns [`VivariumError`] when the run or step rows cannot be read.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn retained(store: &PumpStore, row: &NeedBranchRow) -> Result<Retained, VivariumError> {
    let mut merged = Vec::new();
    let mut no_defect = Vec::new();
    let runs = store.runs_of(&row.need)?;
    for run in &runs {
        if run.run > row.run {
            continue;
        }
        let (mut run_merged, mut run_no_defect) = completed_slices(store, run)?;
        merged.append(&mut run_merged);
        no_defect.append(&mut run_no_defect);
    }
    Ok(Retained {
        target: row.target.clone(),
        branch: row.branch.clone(),
        base: row.base.clone(),
        run: row.run,
        archived_through: archival_boundary(&row.need, row.run, &row.priors),
        merged: by_identity(merged),
        no_defect: by_identity(no_defect),
    })
}

/// The completed slice outcomes of one run, split by verdict.
fn completed_slices(
    store: &PumpStore,
    run: &RunRow,
) -> Result<(Vec<RetainedSlice>, Vec<RetainedSlice>), VivariumError> {
    let mut merged = Vec::new();
    let mut no_defect = Vec::new();
    for row in store.steps_of(&run.need, run.run)? {
        let Some(verdict) = completed_verdict(&row) else {
            continue;
        };
        let word = verdict.to_owned();
        let merged_out = verdict == VERDICT_MERGED;
        let slice = RetainedSlice {
            run: run.run,
            node: row.node,
            task: row.task,
            verdict: word,
        };
        if merged_out {
            merged.push(slice);
        } else {
            no_defect.push(slice);
        }
    }
    Ok((merged, no_defect))
}

/// The verdict of a slice outcome the history keeps, or `None`: the step must
/// be a done slice row closed `merged` or `no-defect`. Unfinished steps and
/// `failed`, `replaced` or `superseded` slice outcomes stay out.
fn completed_verdict(row: &StepRow) -> Option<&str> {
    if row.state != StepState::Done || !is_slice_row(row) {
        return None;
    }
    row.verdict
        .as_deref()
        .filter(|verdict| matches!(*verdict, VERDICT_MERGED | VERDICT_NO_DEFECT))
}

/// The newest run whose prior name is recorded in `priors`: the archival
/// boundary. Slices at or before it moved to that prior branch.
fn archival_boundary(need: &str, owning: u32, priors: &[String]) -> Option<u32> {
    (1..=owning).rev().find(|run| {
        let name = prior(need, *run);
        priors.iter().any(|recorded| *recorded == name)
    })
}

/// The slices in identity order: run, then node, then task.
fn by_identity(mut slices: Vec<RetainedSlice>) -> Vec<RetainedSlice> {
    slices.sort();
    slices
}

/// The worker brief of `history`: target, branch, original base, the already
/// selected `checkout` SHA, and each active slice's source run, node and
/// task. Workers plan or cut remaining work only; the checkout is named,
/// never resolved here.
#[cfg_attr(not(test), allow(dead_code))]
#[must_use]
pub(crate) fn render(history: &Retained, checkout: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("target: {}\n", history.target));
    out.push_str(&format!("branch: {}\n", history.branch));
    if let Some(base) = &history.base {
        out.push_str(&format!("base: {base}\n"));
    }
    out.push_str(&format!("checkout: {checkout}\n"));
    out.push_str("Retained work already on the branch; plan or cut remaining work only.\n");
    for (label, slices) in [
        ("merged", history.active_merged().collect::<Vec<_>>()),
        (
            "unchanged (no-defect, nothing merged)",
            history.active_no_defect().collect::<Vec<_>>(),
        ),
    ] {
        if slices.is_empty() {
            continue;
        }
        out.push_str(&format!("{label}:\n"));
        for slice in slices {
            out.push_str(&format!("  {}\n", source_line(slice)));
        }
    }
    out
}

/// One slice's source identity: run, node, task, verdict.
fn source_line(slice: &RetainedSlice) -> String {
    format!(
        "r{} {} {} {}",
        slice.run, slice.node, slice.task, slice.verdict
    )
}

#[cfg(test)]
#[path = "retained_work_test.rs"]
mod tests;
