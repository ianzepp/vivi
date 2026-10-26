//! `monitor`: a read-only, machine-wide view of running lanes across all
//! mailspaces registered with the pump service.
//!
//! The monitor never claims, stops, or modifies anything; it only observes:
//! no work, no database contents, and no schema ever change. Explicit owner
//! reads of the pump database may create owner-owned WAL coordination files
//! beside it; every other read stays strictly read-only.
//! It never prints secrets: no environment values, no process arguments.
//! Model and provider information comes from vivi's own records, never from
//! inspecting a live process.

pub mod events;
pub mod history;
pub mod lanes;
pub mod model;
pub mod pump;
pub mod slots;
pub mod snapshot;
pub mod tui;

pub use snapshot::{Snapshot, merge_roots, snapshot};
