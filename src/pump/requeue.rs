//! Operator requeue: `vivi pump requeue`, both halves.
//!
//! `vivi pump requeue <step task>...` posts one requeue action per flow step
//! task on its need's open pump record ([`requeue_step`]); the flow engine
//! applies each once per tick ([`crate::pump::flow::operator_actions`]).
//! `vivi pump requeue --job <id>` releases one held pool job instead
//! ([`requeue_job`]).

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

/// `vivi pump requeue <step>...`: post one requeue action per named flow step
/// task on its need's open pump record, for the running pump to apply at its
/// next tick. `--dry-run` prints what would be posted and posts nothing.
///
/// `vivi pump requeue --job <id>` releases one held pool job instead, see
/// [`requeue_job`].
///
/// # Errors
/// Returns [`VivariumError::Message`] for no step, a task that is not a step
/// of an active run, a mailspace without a pump database, or a step whose need
/// has no open pump record (nothing is posted then), and whatever reading the
/// database or posting an action fails with.
pub fn run(project: &Path, cmd: &PumpRequeueCommand) -> Result<(), VivariumError> {
    let home = project_home(project)?;
    if let Some(id) = cmd.job.as_deref() {
        return requeue_job::request(&home, id, cmd.dry_run);
    }
    if cmd.steps.is_empty() {
        return Err(VivariumError::Message(String::from(
            "requeue needs at least one step task handle",
        )));
    }
    let steps = requeue_step::resolve(&home.join(DB_FILENAME), &cmd.steps)?;
    let reason = cmd.reason.as_deref().unwrap_or(DEFAULT_REASON);
    requeue_step::post(project, &steps, reason, cmd.dry_run)
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

/// The line printed once a request is queued.
fn requested_line(task: &str) -> String {
    format!("requeue {task} requested; the pump applies it at its next tick")
}
