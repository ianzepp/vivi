//! The live seam of the band runtime: observing real provider and pool
//! state — the I/O half that a [`super::runtime::Availability`] snapshot
//! summarizes, and the failure classification that turns an observed
//! provider error into [`super::cooldown::Evidence`].
//!
//! This module is a hook point for the live slice; no live I/O lives here
//! yet, and nothing in it may fabricate an observation.
