//! The guard seam of the band runtime: where a dispatch that resolved
//! [`super::runtime::Decision::Ready`] is checked once more against the
//! [`super::runtime::Availability`] snapshot before it is allowed to run.
//!
//! This module is a hook point for the guard slice; no guard logic lives
//! here yet, and nothing in it may answer success without doing the work.
