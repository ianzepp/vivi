//! The pump's flow engine: flows as data, a run per need, one transition point.

pub mod advance;
pub mod answer;
pub mod audit_step;
pub mod collect;
pub mod collect_slice;
pub mod collect_step;
pub mod cto_step;
pub mod decide;
pub mod def;
pub mod fanout;
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
pub mod outcome;
pub mod prune;
pub mod rank;
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
pub mod rework_step;
pub mod rung_step;
pub mod scope;
pub mod slice_dispatch;
pub mod slice_merge;
pub mod step_error;
pub mod tick;
pub mod unlock;
pub mod validate;
pub mod verify;
pub mod verify_inputs;

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
