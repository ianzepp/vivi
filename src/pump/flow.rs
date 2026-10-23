//! The pump's flow engine: flows as data, a run per need, one transition point.

pub mod advance;
pub mod answer;
pub mod collect;
pub mod collect_slice;
pub mod collect_step;
pub mod def;
pub mod fanout;
pub mod handlers;
pub mod index;
pub mod intake;
pub mod kind;
pub mod load;
pub mod mail;
pub mod need_branch;
pub mod outcome;
pub mod prune;
pub mod rank;
pub mod registry;
pub mod run;
pub mod rung_step;
pub mod scope;
pub mod slice_dispatch;
pub mod tick;
pub mod validate;
pub mod verify;

#[cfg(test)]
#[path = "flow/fake_mail_test.rs"]
pub(crate) mod fake_mail;
