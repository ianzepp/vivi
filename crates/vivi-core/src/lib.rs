#![deny(clippy::pedantic)]

//! Mailspace, pump, pools, routing and sync: the vivi core.

pub mod boot;
pub mod cli;
pub mod judgment;
pub mod machine_key;
pub mod mailspace;
pub mod mcp;
pub mod pool;
pub mod pump;
pub mod registry;
pub mod role_schedule;
pub mod role_status;
pub mod routing;
pub mod sync;
