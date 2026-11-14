#![forbid(unsafe_code)]

//! The vivi console: a read-only web view of one `vivi server`, served under
//! `/ui`.
//!
//! The server mounts [`router`] and implements [`ConsoleHost`]. The console
//! reads each project's mailspace store through a read-only connection and
//! never writes. See `docs/console-plan.md`.

mod auth;
pub mod data;
pub mod format;
pub mod host;
pub mod html;
mod layout;
mod pages;
pub mod queries;
mod router;
pub mod session;

#[cfg(test)]
mod test_support;

pub use host::{ConsoleHost, Principal, ProjectRef};
pub use router::router;
