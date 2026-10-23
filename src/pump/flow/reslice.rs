//! Reslice: split, rework and follow-up slicer jobs on an existing slice.

use crate::error::VivariumError;
use crate::mailspace::shipped::SLICER_ROLE;
use crate::pump::flow::advance::start::SENTINEL_NODE;
use crate::pump::flow::def::{NodeClass, STEP_MARKER};
use crate::pump::flow::fanout::SLICE_NODE_PREFIX;
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::step_inputs::{read_only_job, targets};
use crate::pump::host::Ctx;
use crate::pump::jobs::{self, JobSpec, JobStore};
use crate::pump::store::PumpStore;
use crate::pump::store::flow_links::SliceLink;

/// Node-id prefix of a reslice step row.
pub const RESLICE_NODE_PREFIX: &str = "reslice:";

/// Job kind of a reslice slicer job.
pub const KIND: &str = "reslice";

/// What a reslice job asks the slicer to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Split,
    Rework,
    FollowUp,
}

impl Mode {
    /// The slicer-envelope mode word (a follow-up job is the slicer in rework mode).
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Split => "split",
            Self::Rework | Self::FollowUp => "rework",
        }
    }

    /// The mode's own name.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Split => "split",
            Self::Rework => "rework",
            Self::FollowUp => "follow-up",
        }
    }
}

/// Send a slicer job on one slice, in `mode`, with `input` as the ask; returns
/// the reslice task. The same request again sends nothing.
///
/// # Errors
/// Returns a [`VivariumError`] when the run has no sentinel, the slicer role is
/// missing, or a mail, store or job call fails.
#[allow(clippy::too_many_arguments)]
pub(crate) fn request<S: JobStore, M: FlowMail>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    mail: &mut M,
    run: &RunRow,
    slice: &StepRow,
    mode: Mode,
    input: &str,
) -> Result<String, VivariumError> {
    let id = slice
        .node
        .strip_prefix(SLICE_NODE_PREFIX)
        .unwrap_or(&slice.node);
    let rows = store.steps_of(&run.need, run.run)?;
    let prefix = format!("{RESLICE_NODE_PREFIX}{id}-");
    let earlier: Vec<&StepRow> = rows
        .iter()
        .filter(|r| r.node.starts_with(&prefix))
        .collect();
    let tail = format!("mode: {}\nslice: {}\n\n{input}", mode.word(), slice.task);
    for row in &earlier {
        if row.state != StepState::Done && body_tail(&mail.task_body(&row.task)?) == tail {
            return Ok(row.task.clone());
        }
    }
    let node = format!("{prefix}{}", earlier.len() + 1);
    let body = format!("{STEP_MARKER} {} r{} {node}\n{tail}", run.need, run.run);
    let sentinel = sentinel_task(&rows, run)?;
    let subject = format!("{} {id} ({} r{})", mode.name(), run.need, run.run);
    let task = mail.send_step(
        &ctx.cfg.reply_from,
        &ctx.cfg.reply_from,
        &subject,
        &body,
        &[],
    )?;
    mail.bind(&run.need, &[sentinel, task.clone()])?;
    let row = StepRow {
        run: run.run,
        node,
        task: task.clone(),
        state: StepState::Running,
        verdict: None,
    };
    store.insert_step(&run.need, &row)?;
    let fanout = store.slice_link(&slice.task)?.and_then(|link| link.fanout);
    store.link_slice(&SliceLink {
        task: task.clone(),
        need: run.need.clone(),
        run: run.run,
        fanout,
        deps: Vec::new(),
    })?;
    let brief = brief(ctx, mail, slice, mode, input)?;
    let spec = job(ctx, run, &row, slice, mode, &brief)?;
    jobs::enqueue(st, spec)?;
    Ok(task)
}

fn sentinel_task(rows: &[StepRow], run: &RunRow) -> Result<String, VivariumError> {
    rows.iter()
        .find(|r| r.node == SENTINEL_NODE)
        .map(|r| r.task.clone())
        .ok_or_else(|| {
            VivariumError::Parse(format!(
                "need {} r{}: no sentinel step row",
                run.need, run.run
            ))
        })
}

/// The slicer job `reslice-<task>-a1` for a reslice step row.
fn job(
    ctx: &Ctx,
    run: &RunRow,
    row: &StepRow,
    slice: &StepRow,
    mode: Mode,
    brief: &str,
) -> Result<JobSpec, VivariumError> {
    let mut spec = read_only_job(ctx, run, row, &NodeClass::Slicer, SLICER_ROLE, brief)?;
    spec.id = format!("reslice-{}-a1", row.task);
    spec.kind = KIND.to_owned();
    spec.task = Some(row.task.clone());
    spec.payload = serde_json::json!({
        "need": run.need,
        "run": run.run,
        "task": row.task,
        "slice": slice.task,
        "mode": mode.name(),
    });
    Ok(spec)
}

/// The text of a task body from its `mode:` line on.
fn body_tail(body: &str) -> &str {
    body.find("mode: ").map_or("", |at| &body[at..])
}

fn brief<M: FlowMail>(
    ctx: &Ctx,
    mail: &M,
    slice: &StepRow,
    mode: Mode,
    input: &str,
) -> Result<String, VivariumError> {
    Ok(format!(
        "mode: {}\nslice size: aim for 5 to {} minutes\ntargets: {}\n{input}\noriginal slice task:\n{}",
        mode.word(),
        ctx.cfg.slice.minutes(),
        targets(ctx).join(", "),
        mail.task_body(&slice.task)?
    ))
}

/// Registry hook (filled by a later N4 slice).
pub(crate) fn register<S: JobStore, M: FlowMail>(_registry: &mut Registry<S, M>) {}

#[cfg(test)]
#[path = "reslice_test.rs"]
mod tests;
