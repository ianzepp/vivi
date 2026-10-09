#![deny(clippy::pedantic)]

//! Project mailspace for Vivi.
//!
//! This crate owns durable per-project coordination state — tasks, needs,
//! wants, mail, memos, goals, roles, and executable work graphs — plus the
//! `vivi` binary.

pub mod boot;
pub mod chat;
pub mod cli;
pub mod duration;
pub mod error;
pub mod extract;
pub mod judgment;
pub mod mailspace;
pub mod message;
pub mod role_schedule;
pub mod role_status;
pub mod stdout_budget;
pub mod storage;
pub mod store;

pub use error::VivariumError;
