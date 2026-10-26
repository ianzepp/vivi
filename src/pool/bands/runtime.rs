//! Pure runtime resolution over the runtime vocabulary.
//!
//! Hook point for a dependent slice: this module will own pure runtime
//! resolution — answering an [`crate::pool::bands::types::Admission`] for a
//! band from snapshots and cooldowns, with no side effects, using the
//! vocabulary in [`super::types`]. Nothing is defined here yet.
