#![deny(clippy::pedantic)]

//! Project mailspace for Vivi.
//!
//! This crate owns durable per-project coordination state — tasks, needs,
//! wants, mail, memos, rules, goals, roles, and executable work graphs — plus the
//! `vivi` binary.

pub mod b64;
pub mod boot;
pub mod cli;
pub mod doctor;
pub mod duration;
pub mod extract;
pub mod home;
pub mod judgment;
pub mod machine_key;
pub mod mailspace;
pub mod mcp;
#[cfg(unix)]
pub mod mind;
pub mod monitor;
pub mod pool;
pub mod project_create;
pub mod proxy;
pub mod pump;
pub mod registry;
pub mod report;
pub mod role_schedule;
pub mod role_status;
pub mod routing;
pub mod server;
pub mod skills;
pub mod stdout_budget;
pub mod storage;
pub mod store;
pub mod sync;
pub mod update;
