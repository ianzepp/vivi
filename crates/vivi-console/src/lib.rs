#![forbid(unsafe_code)]

//! The vivi console: a read-only web view of one `vivi server`, served under
//! `/ui`.
//!
//! The server mounts [`router`] and implements [`ConsoleHost`], which lists
//! the projects to show. There is no sign-in: the console is for a trusted
//! local network, and it only reads. Each project's stores are opened through
//! connections that cannot write. See `docs/console-plan.md`.

mod context;
pub mod data;
pub mod format;
pub mod host;
pub mod html;
mod layout;
mod pages;
pub mod queries;
mod router;

#[cfg(test)]
mod test_support;

pub use host::{Catalog, ConsoleHost, ProjectRef};
pub use router::router;
