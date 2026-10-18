#![deny(clippy::pedantic)]

//! Project mailspace for Vivi.
//!
//! This crate owns durable per-project coordination state — tasks, needs,
//! wants, mail, memos, rules, goals, roles, and executable work graphs — plus the
//! `vivi` binary.

pub mod b64;
pub mod boot;
pub mod cli;
pub mod duration;
pub mod error;
pub mod extract;
pub mod judgment;
pub mod mailspace;
pub mod mcp;
pub mod message;
pub mod pool;
pub mod proxy;
pub mod pump;
pub mod role_schedule;
pub mod role_status;
pub mod skills;
pub mod stdout_budget;
pub mod storage;
pub mod store;
pub mod sync;
pub mod update;

pub use error::VivariumError;
