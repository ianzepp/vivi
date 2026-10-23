//! Reslice: split, rework and follow-up slicer jobs on an existing slice.

use crate::error::VivariumError;
use crate::mailspace::shipped::SLICER_ROLE;
use crate::pump::envelope::roles::{Slicer, SlicerMode};
use crate::pump::flow::advance::start::SENTINEL_NODE;
use crate::pump::flow::collect_slice::runner_env_failure;
use crate::pump::flow::def::{NodeClass, STEP_MARKER};
use crate::pump::flow::fanout::{SLICE_NODE_PREFIX, SLICES_INVALID, check_batch, parse_reply};
use crate::pump::flow::handlers::{FlowMail, Registry, TickPhase};
use crate::pump::flow::index::run_state;
use crate::pump::flow::index::{JOB_STALE, PayloadIdentity, RunRow, StepRow, StepState};
use crate::pump::flow::reslice_apply;
use crate::pump::flow::step_error::{self, STEP_ERROR_LIMIT};
use crate::pump::flow::step_inputs::{read_only_job, targets};
use crate::pump::flow::tick::{OPERATOR_HOLD, raise_once};
use crate::pump::host::Ctx;
use crate::pump::jobs::rung_result::{ENV_FAILURE, EnvFailure};
use crate::pump::jobs::{self, JobSpec, JobStatus, JobStore, PoolJob};
use crate::pump::store::PumpStore;
use crate::pump::store::flow_links::SliceLink;

/// Node-id prefix of a reslice step row.
pub const RESLICE_NODE_PREFIX: &str = "reslice:";

/// Job kind of a reslice slicer job.
pub const KIND: &str = "reslice";

/// Exception kind of a reslice job that gave no usable slicer reply.
pub const RESLICE_FAILED: &str = "flow-reslice-failed";

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

    /// The mode named `name` (the inverse of [`Mode::name`]).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "split" => Some(Self::Split),
            "rework" => Some(Self::Rework),
            "follow-up" => Some(Self::FollowUp),
            _ => None,
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
        if row.state != StepState::Done && same_text(body_tail(&mail.task_body(&row.task)?), &tail)
        {
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
    let row = run.step(&node, &task, StepState::Running);
    store.insert_run_step(run, &row)?;
    let fanout = store.slice_link(&slice.task)?.and_then(|link| link.fanout);
    store.link_slice(&SliceLink::for_run(run, &task, fanout, Vec::new()))?;
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

/// Whether two texts hold the same lines once each line is trimmed and
/// trailing blank lines are dropped (a mailspace may add a final newline).
fn same_text(a: &str, b: &str) -> bool {
    let lines = |text: &str| -> Vec<String> {
        let mut lines: Vec<String> = text.lines().map(|line| line.trim().to_owned()).collect();
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        lines
    };
    lines(a) == lines(b)
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

/// Registry hook: the reslices phase.
pub(crate) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.phase(Box::new(Reslices));
}

/// Collect finished reslice jobs and apply their result to the run.
struct Reslices;

impl<S: JobStore, M: FlowMail> TickPhase<S, M> for Reslices {
    fn name(&self) -> &'static str {
        "reslices"
    }

    fn run(
        &self,
        ctx: &Ctx,
        st: &mut S,
        store: &PumpStore,
        mail: &mut M,
    ) -> Result<(), VivariumError> {
        let ready: Vec<PoolJob> = st
            .jobs()
            .values()
            .filter(|job| job.kind == KIND && job.status.is_terminal() && job.applied_at.is_none())
            .cloned()
            .collect();
        let mut first_error = None;
        for job in &ready {
            match collect_one(ctx, st, store, mail, job) {
                Ok(()) => step_error::note_success(ctx, &job.id),
                Err(error) => {
                    ctx.log(&format!("reslice job {} failed: {error}", job.id));
                    if let Some(kept) = settle_error(ctx, st, store, job, error) {
                        first_error.get_or_insert(kept);
                    }
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

/// Count one more failing tick of `job`. Below [`STEP_ERROR_LIMIT`] the error
/// is kept for the caller and the job stays unapplied. At the limit the job is
/// marked applied and the reslice row is held for the operator; the error is
/// then settled and gives `None` (a failed hold is the only error returned).
fn settle_error<S: JobStore>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    job: &PoolJob,
    error: VivariumError,
) -> Option<VivariumError> {
    let n = step_error::note_failure(ctx, &job.id);
    if n < STEP_ERROR_LIMIT {
        return Some(error);
    }
    jobs::mark_applied(st, &job.id, ctx.now());
    let task = job.payload.get("task").and_then(|v| v.as_str())?;
    let text = format!(
        "reslice job {} for {task}: expected the reply to apply; actual: {error} on {n} ticks in a row",
        job.id
    );
    step_error::hold(ctx, store, task, &text).err()
}

/// The slicer agent could not run: the reply says nothing about the slice.
/// Alarm once, hold the reslice row for the operator, settle the job. The
/// slice and its attempts are untouched.
fn hold_env<S: JobStore>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    job: &PoolJob,
    row: &StepRow,
    env: &EnvFailure,
) -> Result<(), VivariumError> {
    let slice = job.payload.get("slice").and_then(|v| v.as_str());
    raise_once(
        ctx,
        ENV_FAILURE,
        &job.id,
        &format!(
            "reslice job {} for {}: expected the slicer agent to run; actual: {:?}, tool {}: {}; \
             answer with vivi pump answer {} <word>{}",
            job.id,
            row.task,
            env.kind,
            env.tool.as_deref().unwrap_or("none"),
            env.evidence,
            row.task,
            slice.map_or_else(String::new, |s| format!(" (slice {s})"))
        ),
    );
    store.set_step_state(&row.task, StepState::Waiting, Some(OPERATOR_HOLD))?;
    jobs::mark_applied(st, &job.id, ctx.now());
    Ok(())
}

/// The live run and the step row a reslice job names, or `None` when the job
/// is settled here. A payload identity that is missing, names no live run or
/// no step row, or disagrees with the step row's run is stale: the job is
/// marked applied and [`JOB_STALE`] is raised once.
fn checked_identity<S: JobStore>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    job: &PoolJob,
) -> Result<Option<(RunRow, StepRow)>, VivariumError> {
    let stale = |st: &mut S, text: String| {
        raise_once(ctx, JOB_STALE, &job.id, &text);
        jobs::mark_applied(st, &job.id, ctx.now());
        Ok(None)
    };
    let identity = match PayloadIdentity::of(&job.payload) {
        Ok(identity) => identity,
        Err(error) => return stale(st, format!("reslice job {}: {error}", job.id)),
    };
    let runs = store.runs_of(&identity.need)?;
    let Some(run) = runs
        .iter()
        .find(|r| r.run == identity.run && r.state == run_state::LIVE)
        .cloned()
    else {
        let live: Vec<String> = runs
            .iter()
            .filter(|r| r.state == run_state::LIVE)
            .map(|r| format!("r{}", r.run))
            .collect();
        let found = if live.is_empty() {
            format!("no live run of {}", identity.need)
        } else {
            format!("live runs of {}: {}", identity.need, live.join(", "))
        };
        let text = format!(
            "reslice job {}: expected run r{} of {} live; actual: {found}",
            job.id, identity.run, identity.need
        );
        return stale(st, text);
    };
    let Some(row) = store.step_by_task(&identity.task)? else {
        let text = format!(
            "reslice job {}: expected task {} to have a step row; actual: no such step",
            job.id, identity.task
        );
        return stale(st, text);
    };
    if row.run != identity.run {
        let text = format!(
            "reslice job {}: expected run r{} (the payload); actual: step {} belongs to r{}",
            job.id, identity.run, row.task, row.run
        );
        return stale(st, text);
    }
    Ok(Some((run, row)))
}

/// Apply one terminal reslice job, or drop it when its run is closed.
fn collect_one<S: JobStore, M: FlowMail>(
    ctx: &Ctx,
    st: &mut S,
    store: &PumpStore,
    mail: &mut M,
    job: &PoolJob,
) -> Result<(), VivariumError> {
    let Some((run, row)) = checked_identity(ctx, st, store, job)? else {
        return Ok(());
    };
    let text = |key: &str| job.payload.get(key).and_then(|v| v.as_str());
    let (Some(slice), Some(mode)) = (text("slice"), text("mode").and_then(Mode::from_name)) else {
        jobs::mark_applied(st, &job.id, ctx.now());
        return Err(VivariumError::Parse(format!(
            "reslice job {}: payload lacks slice or mode",
            job.id
        )));
    };
    if matches!(row.state, StepState::Done | StepState::Failed) {
        ctx.log(&format!(
            "reslice job {}: {} is already closed; late result dropped",
            job.id, row.task
        ));
        jobs::mark_applied(st, &job.id, ctx.now());
        return Ok(());
    }
    if let Some(env) = runner_env_failure(job) {
        return hold_env(ctx, st, store, job, &row, &env);
    }
    let reply = reply_of(ctx, store, &run, job, mode)?;
    let word = reslice_apply::apply(ctx, store, mail, &run, &row, slice, mode, reply.as_ref())?;
    mail.close_task(&row.task, word, None, None)?;
    store.set_step_state(&row.task, StepState::Done, Some(word))?;
    jobs::mark_applied(st, &job.id, ctx.now());
    Ok(())
}

/// The slicer envelope of a finished job whose mode matches the request; any
/// other outcome raises [`RESLICE_FAILED`] and gives `None`, and slices that
/// fail [`check_batch`] raise [`SLICES_INVALID`] and give `None`. A store read
/// error from [`check_batch`] is an Err.
fn reply_of(
    ctx: &Ctx,
    store: &PumpStore,
    run: &RunRow,
    job: &PoolJob,
    mode: Mode,
) -> Result<Option<Slicer>, VivariumError> {
    let fail = |why: String| {
        let text = format!("reslice job {}: {why}", job.id);
        raise_once(ctx, RESLICE_FAILED, &job.id, &text);
        Ok(None)
    };
    if job.status != JobStatus::Finished {
        return fail(format!("pool job ended {:?}", job.status));
    }
    let Some(outcome) = &job.outcome else {
        return fail("no outcome saved".to_owned());
    };
    let message = crate::pump::attempt_record::hand_message(&outcome.receipt);
    let slicer = match parse_reply(&message) {
        Ok(slicer) => slicer,
        Err(error) => return fail(format!("unusable reply: {error}")),
    };
    let wanted = if mode == Mode::Split {
        SlicerMode::Split
    } else {
        SlicerMode::Rework
    };
    if slicer.mode != wanted {
        return fail(format!("reply mode {:?} is not {wanted:?}", slicer.mode));
    }
    if let Some(reason) = check_batch(store, run, &slicer.slice)? {
        let text = format!("reslice job {}: {reason}", job.id);
        raise_once(ctx, SLICES_INVALID, &job.id, &text);
        return Ok(None);
    }
    Ok(Some(slicer))
}

#[cfg(test)]
#[path = "reslice_test.rs"]
mod tests;

#[cfg(test)]
#[path = "reslice_mailspace_test.rs"]
mod mailspace_tests;
