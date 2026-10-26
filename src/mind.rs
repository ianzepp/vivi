//! `vivi mind`: one persistent, hub-wide supervisor over every registered pump.
//!
//! EXPLORATORY SPIKE. See `docs/design-vivi-mind.md` for the design, what is
//! real, what is stubbed, and the open questions.
//!
//! The mind is a long-lived service, independent of every pump: it is not a
//! pump cadence job, so a deadlocked or crashed pump cannot take it down. It
//! wakes on a heartbeat and on pump signals, judges what it sees inside one
//! persistent pi harness session, and acts only through letters and the pump
//! controls the pump already accepts, gated by an operator-chosen
//! [`config::AuthorityStage`]. An operator attaches with `vivi mind` through
//! a Unix socket whose file permissions are the access gate.

pub mod actions;
pub mod authority;
pub mod commands;
pub mod config;
pub mod pi;
pub mod probe;
pub mod projects;
pub mod prompt;
pub mod service;
pub mod socket;
pub mod store;
pub mod wake;

pub use commands::dispatch as run;
