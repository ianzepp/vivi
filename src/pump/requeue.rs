//! Operator requeue: `vivi pump requeue`, both halves.
//!
//! `vivi pump requeue <step task>...` posts one requeue action per flow step
//! task on its need's open pump record ([`requeue_step`]); the flow engine
//! applies each once per tick ([`crate::pump::flow::operator_actions`]).
//! `vivi pump requeue --job <id>` releases one held pool job instead
//! ([`requeue_job`]).
//!
//! [`requeue`] returns what it posted, or what a dry run would post, and does
//! not print. [`run`] is the CLI wrapper: it prints each result.

use std::fmt;
use std::path::Path;

use crate::VivariumError;
use crate::cli::PumpRequeueCommand;
use crate::pump::control::project_home;
use crate::pump::state::State;
use crate::pump::store::{DB_FILENAME, PumpStore};

#[path = "requeue_job.rs"]
mod requeue_job;
#[path = "requeue_step.rs"]
mod requeue_step;

/// The reason a request carries when the operator gave none.
const DEFAULT_REASON: &str = "operator requeue";

/// What [`requeue`] acts on: flow step tasks, or one held pool job.
#[derive(Debug, Copy, Clone)]
pub enum RequeueTarget<'a> {
    /// Step task handles. An absent `reason` posts `operator requeue`.
    Steps {
        tasks: &'a [String],
        reason: Option<&'a str>,
    },
    /// One held pool job.
    Job(&'a str),
}

/// One requeue [`requeue`] performed, or would perform on a dry run.
///
/// Display is the line the CLI prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequeueResult {
    /// A step requeue posted on the need's open pump record.
    Step {
        task: String,
        need: String,
        /// The open pump record the action was posted on.
        record: String,
        /// The action letter's content id.
        id: String,
    },
    /// A step requeue a dry run would post.
    StepPreview { task: String, need: String },
    /// A release request written for a held job. `id` is the job id.
    Job { id: String },
    /// A held-job release a dry run would request. `id` is the job id.
    JobPreview { id: String },
}

impl fmt::Display for RequeueResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StepPreview { task, need } => {
                write!(f, "would requeue flow step {task} of need {need}")
            }
            Self::Step { task, .. } => write!(
                f,
                "requeue {task} requested; the pump applies it at its next tick"
            ),
            Self::JobPreview { id } => write!(f, "would request release of job {id}"),
            Self::Job { id } => write!(
                f,
                "release of job {id} requested; the pump applies it at its next tick"
            ),
        }
    }
}

/// Post one requeue per step task, or request release of one held job.
///
/// A job target wins when the caller has one: steps are not read. A dry run
/// changes nothing and returns the preview results. Nothing is printed.
///
/// # Errors
/// Returns [`VivariumError::Message`] for no step, a task that is not a step
/// of an active run, a mailspace without a pump database, a step whose need
/// has no open pump record (nothing is posted then), an unknown job, or a job
/// that is not held, and whatever reading the database or posting an action
/// fails with.
pub fn requeue(
    project: &Path,
    target: &RequeueTarget<'_>,
    dry_run: bool,
) -> Result<Vec<RequeueResult>, VivariumError> {
    let home = project_home(project)?;
    match *target {
        RequeueTarget::Job(id) => Ok(vec![requeue_job::request(&home, id, dry_run)?]),
        RequeueTarget::Steps { tasks, reason } => {
            requeue_steps(project, &home, tasks, reason, dry_run)
        }
    }
}

fn requeue_steps(
    project: &Path,
    home: &Path,
    tasks: &[String],
    reason: Option<&str>,
    dry_run: bool,
) -> Result<Vec<RequeueResult>, VivariumError> {
    if tasks.is_empty() {
        return Err(VivariumError::Message(String::from(
            "requeue needs at least one step task handle",
        )));
    }
    let steps = requeue_step::resolve(&home.join(DB_FILENAME), tasks)?;
    let reason = reason.unwrap_or(DEFAULT_REASON);
    requeue_step::post(project, &steps, reason, dry_run)
}

/// `vivi pump requeue`: [`requeue`], then print each result.
///
/// # Errors
/// Returns whatever [`requeue`] returns.
pub fn run(project: &Path, cmd: &PumpRequeueCommand) -> Result<(), VivariumError> {
    let target = match cmd.job.as_deref() {
        Some(id) => RequeueTarget::Job(id),
        None => RequeueTarget::Steps {
            tasks: &cmd.steps,
            reason: cmd.reason.as_deref(),
        },
    };
    for done in requeue(project, &target, cmd.dry_run)? {
        println!("{done}");
    }
    Ok(())
}

/// Load the pump state at `path`, refusing a mailspace with no database.
fn load_state(path: &Path) -> Result<State, VivariumError> {
    let Some(store) = PumpStore::open_read_only(path)? else {
        return Err(VivariumError::Message(format!(
            "no pump.db at {}",
            path.display()
        )));
    };
    store.load()
}
