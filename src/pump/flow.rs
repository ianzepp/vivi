//! The pump's flow engine: flows as data, a run per need, one transition point.

pub mod advance;
pub mod collect;
pub mod def;
pub mod fanout;
pub mod index;
pub mod load;
pub mod mail;
pub mod outcome;
pub mod registry;
pub mod run;
pub mod slice_dispatch;
pub mod tick;
pub mod validate;

#[cfg(test)]
#[path = "flow/fake_mail_test.rs"]
pub(crate) mod fake_mail;
