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
pub mod need_branch;
pub mod outcome;
pub mod prune;
pub mod rank;
pub mod registry;
pub mod residual;
pub mod rework_step;
pub mod rung_step;
pub mod scope;
pub mod slice_dispatch;
pub mod slice_merge;
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
