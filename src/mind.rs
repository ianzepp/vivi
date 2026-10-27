//! `vivi mind`: one persistent, hub-wide supervisor over every registered pump.
//!
//! The mind is a long-lived service, independent of every pump: it is not a
//! pump cadence job, so a deadlocked or crashed pump cannot take it down. It
//! wakes on a heartbeat and on pump signals, judges what it sees inside one
//! persistent pi harness session, and acts only through the mail server's
//! MCP tools — one tool call per permitted action, each gated by an
//! operator-chosen [`config::AuthorityStage`] and a per-need or per-project
//! budget. Without an `[mcp]` server in `mind.toml`, execution is
//! deliberately a dry run that only logs what would run. This build opens
//! the `observe`, `unstick` and `file-fixes` stages; the later
//! `upgrade-on-approval` and `self-release` stages parse but are refused by
//! the config loader. An operator attaches with `vivi mind` through a Unix
//! socket whose file permissions are the access gate. See
//! `docs/design-vivi-mind.md` for the action contract and what remains
//! stubbed.

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
