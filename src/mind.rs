//! `vivi mind`: one persistent, hub-wide supervisor over every registered pump.
//!
//! See `docs/design-vivi-mind.md` for the delivered action contract, the
//! operating sequence, and what remains stubbed.
//!
//! The mind is a long-lived service, independent of every pump: it is not a
//! pump cadence job, so a deadlocked or crashed pump cannot take it down. It
//! wakes on a heartbeat and on pump signals, judges what it sees inside one
//! persistent pi harness session, and decides what happens to each action in
//! a turn's closing TOML envelope: [`actions`] parses and bounds it,
//! [`authority`] gates it by the operator-chosen [`config::AuthorityStage`],
//! the pause state, and the per-need and per-project budgets, and
//! [`mcp_actuator`] carries each permitted action out as exactly one call to
//! the hub server's MCP tools. Without an `[mcp]` server in `mind.toml` the
//! same gate runs and a dry run only records what would run. An operator
//! attaches with `vivi mind` through a Unix socket whose file permissions
//! are the access gate.

pub mod actions;
pub mod authority;
pub mod commands;
pub mod config;
pub mod mcp_actuator;
pub mod pi;
pub mod probe;
pub mod projects;
pub mod prompt;
pub mod service;
pub mod socket;
pub mod store;
pub mod wake;

pub use commands::dispatch as run;
