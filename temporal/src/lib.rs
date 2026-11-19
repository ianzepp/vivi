//! Vivi's need pipeline on Temporal: the workflows, the activity interface,
//! the real activities that wrap Vivi's commands, and the stand-ins that
//! replay a scripted fault model for the tests. See README.md.

pub mod activities;
pub mod admission;
pub mod classify;
pub mod config;
pub mod defs;
pub mod error;
pub mod fault;
pub mod lander;
pub mod need;
pub mod real;
pub mod record;
pub mod routing;
pub mod slice;
pub mod steps;
pub mod types;
pub mod worker;
pub mod world;

use temporalio_client::{WorkflowIdReusePolicy, WorkflowStartOptions};
use temporalio_sdk::WorkerOptions;
use temporalio_sdk::workflows::WorkflowRegistrationError;

use crate::activities::Stand;
use crate::lander::LanderWorkflow;
use crate::need::NeedWorkflow;
use crate::slice::SliceWorkflow;

pub fn need_workflow_id(handle: &str) -> String {
    format!("need-{handle}")
}

pub fn slice_workflow_id(need: &str, slice: &str) -> String {
    format!("slice-{need}-{slice}")
}

/// Start options for a need. The id is `need-<handle>` and the server
/// refuses a second start, whether the first run is still going or done.
pub fn need_start_options(task_queue: &str, handle: &str) -> WorkflowStartOptions {
    WorkflowStartOptions::new(task_queue, need_workflow_id(handle))
        .id_reuse_policy(WorkflowIdReusePolicy::RejectDuplicate)
        .build()
}

/// Start options for an asked-for rerun of a need whose last run has
/// ended (parked, terminated, or closed). A run still going is refused all
/// the same: the server never runs two with one id.
pub fn need_rerun_options(task_queue: &str, handle: &str) -> WorkflowStartOptions {
    WorkflowStartOptions::new(task_queue, need_workflow_id(handle))
        .id_reuse_policy(WorkflowIdReusePolicy::AllowDuplicate)
        .build()
}

/// One worker that runs all three workflows and the stand-in activities on
/// one queue (the tests). A deployment uses `worker::build`.
pub fn worker_options(
    task_queue: &str,
    stand: Stand,
) -> Result<WorkerOptions, WorkflowRegistrationError> {
    Ok(WorkerOptions::new(task_queue)
        .register_workflow::<NeedWorkflow>()?
        .register_workflow::<SliceWorkflow>()?
        .register_workflow::<LanderWorkflow>()?
        .register_activities(stand)
        .build())
}
