//! Fanout: the slicer's `[[slice]]` tables become slice tasks, and the
//! fanout settles `built` or `failed` from the slices' state.
//!
//! A slice's step row has the node `slice:<id>`. That node is never a node of
//! a flow, so the router's sentinel lookup (the run's first row whose node is
//! not a flow node) still finds the sentinel, which is inserted first.
//!
//! Exhaustion is recorded as step state `Failed` on the slice row
//! ([`mark_exhausted`]). A blocked-scope answer never sets it.

use std::collections::BTreeMap;

use crate::error::VivariumError;
use crate::pump::envelope::roles::{Slice, Slicer};
use crate::pump::envelope::{EnvelopeError, parse};
use crate::pump::flow::advance::start::SENTINEL_NODE;
use crate::pump::flow::def::{FAILED, STEP_MARKER};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::mail::{MailReader, MailWriter, MailspacePort, TaskStatus};
use crate::pump::flow::scope::ScopeState;
use crate::pump::host::Ctx;
use crate::pump::store::PumpStore;

/// Prefix of a slice step row's node: `slice:<id>`.
pub const SLICE_NODE_PREFIX: &str = "slice:";

/// The word the fanout answers when every slice is built.
pub const BUILT: &str = "built";

/// A slice task closed because its work merged onto the need branch.
pub const VERDICT_MERGED: &str = "merged";

/// A slice task closed because its hand found nothing to change.
pub const VERDICT_NO_DEFECT: &str = "no-defect";

/// A slice task closed because a split replaced it; its replacements count.
const VERDICT_REPLACED: &str = crate::pump::flow::def::VERDICT_REPLACED;

/// Parse a slicer reply. This is a structural parse only: the semantic
/// checks (acyclic deps, non-empty scopes, known targets) belong to the
/// build flow's slicer-output validation.
///
/// # Errors
/// Returns [`EnvelopeError`] when the reply is not a valid slicer envelope.
pub fn parse_reply(text: &str) -> Result<Slicer, EnvelopeError> {
    parse::<Slicer>(text).map(|envelope| envelope.front)
}

/// The node of the step row for slice `id`.
#[must_use]
pub fn slice_node(id: &str) -> String {
    format!("{SLICE_NODE_PREFIX}{id}")
}

/// True when `row` is a slice task's step row.
#[must_use]
pub fn is_slice_row(row: &StepRow) -> bool {
    row.node.starts_with(SLICE_NODE_PREFIX)
}

/// The `target:` line of a slice task body, if it has one.
#[must_use]
pub fn target_of(body: &str) -> Option<&str> {
    body.lines()
        .find_map(|line| line.strip_prefix("target:"))
        .map(str::trim)
        .filter(|target| !target.is_empty())
}

/// Send the slice tasks of `slices` in topological order and index them.
///
/// Each task depends on the slicer step's task and on the tasks of its
/// prerequisites (slices of this batch, or slices an earlier fanout of the
/// run already sent). The tasks are bound to the need with the run sentinel
/// in one call, then each slice gets a pending step row and a `slice_scope`
/// row. Nothing is sent when the batch is refused.
///
/// Returns the sent task handles in the order they were sent.
///
/// # Errors
/// Returns a [`VivariumError`] for a duplicate slice id, a dependency on an
/// unknown slice, a dependency cycle, a run without a sentinel, or a failed
/// mail or store call.
pub fn expand<M: MailWriter>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &mut M,
    run: &RunRow,
    slicer_step: &StepRow,
    slices: &[Slice],
) -> Result<Vec<String>, VivariumError> {
    let rows = store.steps_of(&run.need, run.run)?;
    let sentinel = sentinel_of(&rows, run)?;
    let order = topological_order(slices, &rows)?;

    let mut sent: BTreeMap<&str, String> = BTreeMap::new();
    let mut pairs: Vec<(&Slice, String)> = Vec::with_capacity(order.len());
    for slice in order {
        let mut depends_on = vec![slicer_step.task.clone()];
        for dep in &slice.deps {
            let task = match sent.get(dep.as_str()) {
                Some(task) => task.clone(),
                None => earlier_task(&rows, dep)?,
            };
            if !depends_on.contains(&task) {
                depends_on.push(task);
            }
        }
        let task = mail.send_step(
            &ctx.cfg.reply_from,
            &ctx.cfg.reply_from,
            &format!("{} ({} r{})", slice.title, run.need, run.run),
            &slice_body(run, slice),
            &depends_on,
        )?;
        sent.insert(slice.id.as_str(), task.clone());
        pairs.push((slice, task));
    }

    let mut units = vec![sentinel];
    units.extend(pairs.iter().map(|(_, task)| task.clone()));
    mail.bind(&run.need, &units)?;

    for (slice, task) in &pairs {
        record_slice(store, run, slice, task)?;
    }
    Ok(pairs.into_iter().map(|(_, task)| task).collect())
}

fn sentinel_of(rows: &[StepRow], run: &RunRow) -> Result<String, VivariumError> {
    rows.iter()
        .find(|row| row.node == SENTINEL_NODE)
        .map(|row| row.task.clone())
        .ok_or_else(|| {
            VivariumError::Parse(format!(
                "need {} r{}: no sentinel step row",
                run.need, run.run
            ))
        })
}

/// Write a slice's pending step row and its `slice_scope` row.
fn record_slice(
    store: &PumpStore,
    run: &RunRow,
    slice: &Slice,
    task: &str,
) -> Result<(), VivariumError> {
    store.insert_step(
        &run.need,
        &StepRow {
            run: run.run,
            node: slice_node(&slice.id),
            task: task.to_string(),
            state: StepState::Pending,
            verdict: None,
        },
    )?;
    store.insert_slice_scope(task, run.run, &slice.write_scope)
}

fn slice_body(run: &RunRow, slice: &Slice) -> String {
    format!(
        "{STEP_MARKER} {} r{} {}\ntarget: {}\n\n{}\n\n{}",
        run.need,
        run.run,
        slice_node(&slice.id),
        slice.target,
        slice.title,
        slice.brief
    )
}

/// The task of the latest slice an earlier fanout of the run sent for `dep`.
fn earlier_task(rows: &[StepRow], dep: &str) -> Result<String, VivariumError> {
    let node = slice_node(dep);
    rows.iter()
        .rev()
        .find(|row| row.node == node)
        .map(|row| row.task.clone())
        .ok_or_else(|| VivariumError::Parse(format!("slice dependency `{dep}` is not a slice")))
}

/// The batch in an order where every in-batch prerequisite comes first,
/// keeping the slicer's order otherwise. Refuses duplicate ids, unknown
/// dependencies and cycles.
fn topological_order<'a>(
    slices: &'a [Slice],
    rows: &[StepRow],
) -> Result<Vec<&'a Slice>, VivariumError> {
    for (index, slice) in slices.iter().enumerate() {
        if slices[..index].iter().any(|other| other.id == slice.id) {
            return Err(VivariumError::Parse(format!(
                "slice id `{}` appears twice",
                slice.id
            )));
        }
    }
    for slice in slices {
        for dep in &slice.deps {
            let in_batch = slices.iter().any(|other| other.id == *dep);
            if dep == &slice.id {
                return Err(VivariumError::Parse(format!(
                    "slice `{}` depends on itself",
                    slice.id
                )));
            }
            if !in_batch {
                earlier_task(rows, dep)?;
            }
        }
    }
    let mut placed: Vec<&Slice> = Vec::with_capacity(slices.len());
    while placed.len() < slices.len() {
        let next = slices.iter().find(|slice| {
            !placed.iter().any(|done| done.id == slice.id)
                && slice.deps.iter().all(|dep| {
                    placed.iter().any(|done| done.id == *dep)
                        || !slices.iter().any(|other| other.id == *dep)
                })
        });
        match next {
            Some(slice) => placed.push(slice),
            None => {
                return Err(VivariumError::Parse(
                    "slice dependencies form a cycle".to_string(),
                ));
            }
        }
    }
    Ok(placed)
}

/// Close a slice task with `verdict` and mark its step row done.
///
/// This is the one way a slice closes: the dispatchers only start slices and
/// collect their jobs, and the fanout reports the result upward through
/// [`settle`]. `verdict` is [`VERDICT_NO_DEFECT`] (or [`VERDICT_MERGED`]).
///
/// # Errors
/// Returns a [`VivariumError`] when the close or the index write fails.
pub fn close_slice<M: MailWriter>(
    store: &PumpStore,
    mail: &mut M,
    task: &str,
    verdict: &str,
) -> Result<(), VivariumError> {
    mail.close_task(task, verdict, None, None)?;
    store.set_step_state(task, StepState::Done, Some(verdict))
}

/// The mailspace write a slice's dependency edge needs.
pub trait SliceEdges {
    /// Make `task` depend on `depends_on`, after both were sent.
    ///
    /// # Errors
    /// Returns [`VivariumError`] when the edge cannot be written.
    fn add_dependency(&mut self, task: &str, depends_on: &str) -> Result<(), VivariumError>;
}

impl SliceEdges for MailspacePort {
    fn add_dependency(&mut self, task: &str, depends_on: &str) -> Result<(), VivariumError> {
        self.mailspace
            .set_dependencies(
                &self.pump_identity,
                task,
                "task",
                &[depends_on.to_string()],
                &[],
            )
            .map(|_| ())
    }
}

#[cfg(test)]
impl SliceEdges for crate::pump::flow::fake_mail::FakeMail {
    fn add_dependency(&mut self, task: &str, depends_on: &str) -> Result<(), VivariumError> {
        let entry = self
            .tasks
            .get_mut(task)
            .ok_or_else(|| VivariumError::Message(format!("fake mail: no task {task}")))?;
        entry.depends_on.push(depends_on.to_string());
        Ok(())
    }
}

/// Record that a slice ran out of attempts. The fanout then settles `failed`.
///
/// # Errors
/// Returns a [`VivariumError`] when the index cannot be written.
pub fn mark_exhausted(store: &PumpStore, task: &str) -> Result<(), VivariumError> {
    store.set_step_state(task, StepState::Failed, Some(FAILED))
}

/// Where the run's fanout stands: `Some("built")` when every slice task is
/// closed `merged` or `no-defect`, `Some("failed")` when any slice is
/// exhausted, otherwise `None`. A run with no slices is `None`.
///
/// A slice waiting on blocked-scope resolution (`scope-wait` or
/// `scope-repair`) is neither built nor failed.
///
/// # Errors
/// Returns a [`VivariumError`] when the index or the mailspace cannot be read.
pub fn settle<M: MailReader>(
    store: &PumpStore,
    mail: &M,
    run: &RunRow,
) -> Result<Option<&'static str>, VivariumError> {
    let mut slices = 0;
    let mut unsettled = false;
    for row in store.steps_of(&run.need, run.run)? {
        if !is_slice_row(&row) {
            continue;
        }
        if row.state == StepState::Failed {
            return Ok(Some(FAILED));
        }
        let state = mail.task_state(&row.task)?;
        let closed = state.status == TaskStatus::Done;
        let verdict = state.verdict.as_deref();
        if closed && verdict == Some(VERDICT_REPLACED) {
            continue;
        }
        slices += 1;
        let waiting = matches!(
            store.scope_state(&row.task)?,
            Some(ScopeState::Wait | ScopeState::Repair)
        );
        let built = closed && matches!(verdict, Some(VERDICT_MERGED | VERDICT_NO_DEFECT));
        if waiting || !built {
            unsettled = true;
        }
    }
    Ok((slices > 0 && !unsettled).then_some(BUILT))
}

#[cfg(test)]
#[path = "fanout_test.rs"]
mod tests;
