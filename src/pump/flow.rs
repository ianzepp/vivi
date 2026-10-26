//! The pump's flow engine: flows as data, a run per need, one transition point.

pub mod advance;
pub mod answer;
pub mod audit_step;
pub(crate) mod budget_retry;
pub mod build_front;
pub mod collect;
pub mod collect_slice;
pub mod collect_step;
pub mod cto_step;
pub mod decide;
pub mod def;
pub mod fanout;
pub mod follow_up;
pub mod handlers;
pub mod index;
pub mod intake;
pub mod integration;
pub mod judge_step;
pub mod kind;
pub mod load;
pub mod mail;
pub mod main_rung;
pub mod milestone;
pub mod need_branch;
pub mod operator_actions;
pub mod outcome;
pub mod prune;
pub mod rank;
pub mod ready_slices;
pub mod registry;
pub mod repair;
pub mod repair_audit;
pub mod repair_collect;
pub mod repair_start;
pub mod repair_steps;
pub mod repair_unrepaired;
pub mod research;
pub mod research_post;
pub mod residual;
pub mod reslice;
pub mod reslice_apply;
pub mod rework_step;
pub mod rung_step;
pub mod scope;
pub mod slice_dispatch;
pub mod slice_merge;
pub mod step_error;
pub mod step_inputs;
pub mod step_pending;
pub mod thread_change;
pub mod tick;
pub mod unlock;
pub mod validate;
pub mod verify;
pub mod verify_inputs;

use std::path::Path;

use crate::VivariumError;
use crate::mailspace::Mailspace;

use super::config::PumpConfig;
use super::store::{DB_FILENAME, PumpStore};

use self::def::FlowKind;
use self::index::{RunRow, StepRow};

/// The `flow show` answer for one flow kind: the shipped Mermaid source and,
/// when a need was asked for, that need's runs with their steps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowShow {
    /// The requested kind.
    pub kind: FlowKind,
    /// The shipped Mermaid source of the flow.
    pub mermaid: String,
    /// `None` when no need was requested; `Some(None)` when the project has
    /// no pump.db; `Some(Some(runs))` with each run's steps in stored order.
    pub runs: Option<Option<Vec<(RunRow, Vec<StepRow>)>>>,
}

/// The `flow show` view of `kind`, plus the runs and steps of `need` when one
/// is given. Reads the pump database read-only and never prints.
///
/// # Errors
/// Returns [`VivariumError`] when `kind` has no shipped flow or the run index
/// cannot be read.
pub fn view(project: &Path, kind: FlowKind, need: Option<&str>) -> Result<FlowShow, VivariumError> {
    let mermaid = registry::shipped(kind)
        .ok_or_else(|| {
            VivariumError::Message(format!("the {} flow is not shipped yet", kind.name()))
        })?
        .to_owned();
    let runs = match need {
        Some(need) => Some(run_index(need, project)?),
        None => None,
    };
    Ok(FlowShow {
        kind,
        mermaid,
        runs,
    })
}

/// The runs of `need` with their steps, or `None` when there is no pump.db.
fn run_index(
    need: &str,
    project: &Path,
) -> Result<Option<Vec<(RunRow, Vec<StepRow>)>>, VivariumError> {
    let cfg = match Mailspace::discover(Some(project)) {
        Ok(mailspace) => mailspace.config.pump.clone().unwrap_or_default(),
        Err(_) => PumpConfig::default(),
    };
    let path = cfg.home_dir(project).join(DB_FILENAME);
    let Some(store) = PumpStore::open_read_only(&path)? else {
        return Ok(None);
    };
    let mut runs = Vec::new();
    for run in store.runs_of(need)? {
        let steps = store.steps_of(need, run.run)?;
        runs.push((run, steps));
    }
    Ok(Some(runs))
}

#[cfg(test)]
#[path = "flow/fake_mail_test.rs"]
pub(crate) mod fake_mail;

#[cfg(test)]
#[path = "flow/verify_flow_test.rs"]
mod verify_flow_test;

#[cfg(test)]
#[path = "flow/raise_flow_test.rs"]
mod raise_flow_test;

#[cfg(test)]
#[path = "flow/cto_flow_test.rs"]
mod cto_flow_test;

#[cfg(test)]
#[path = "flow/e2e_rig_test.rs"]
pub(crate) mod e2e_rig;

#[cfg(test)]
#[path = "flow/e2e_research_test.rs"]
mod e2e_research_test;

#[cfg(test)]
#[path = "flow/e2e_release_test.rs"]
mod e2e_release_test;

#[cfg(test)]
#[path = "flow/e2e_repair_fix_test.rs"]
mod e2e_repair_fix_test;

#[cfg(test)]
#[path = "flow/e2e_repair_land_test.rs"]
mod e2e_repair_land_test;

#[cfg(test)]
#[path = "flow/e2e_repair_green_test.rs"]
mod e2e_repair_green_test;

#[cfg(test)]
#[path = "flow/e2e_build_test.rs"]
mod e2e_build_test;

#[cfg(test)]
#[path = "flow/e2e_gate_full_test.rs"]
mod e2e_gate_full_test;

#[cfg(test)]
#[path = "flow/e2e_gate_front_test.rs"]
mod e2e_gate_front_test;

#[cfg(test)]
#[path = "flow/e2e_gate_admit_test.rs"]
mod e2e_gate_admit_test;

#[cfg(test)]
#[path = "flow/e2e_gate_verify_test.rs"]
mod e2e_gate_verify_test;

#[cfg(test)]
#[path = "flow/e2e_gate_rework_test.rs"]
mod e2e_gate_rework_test;

#[cfg(test)]
#[path = "flow/e2e_gate_integration_test.rs"]
mod e2e_gate_integration_test;

#[cfg(test)]
#[path = "flow/e2e_gate_cto_test.rs"]
mod e2e_gate_cto_test;

#[cfg(test)]
#[path = "flow/e2e_gate_repair_test.rs"]
mod e2e_gate_repair_test;

#[cfg(test)]
#[path = "flow/e2e_cross_need_test.rs"]
mod e2e_cross_need_test;

#[cfg(test)]
#[path = "flow_test.rs"]
mod tests;
