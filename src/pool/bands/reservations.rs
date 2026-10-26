//! Slot reservations: one row per in-flight claim a job holds on a pool's
//! model.
//!
//! Foundation only: the table that stores these rows and the claiming that
//! writes them arrive with later slices.

use serde::{Deserialize, Serialize};

/// One in-flight claim a job holds on a pool's model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReservationRow {
    /// The pool whose slot is held.
    pub pool: String,
    /// The job holding the slot.
    pub job: String,
    /// The model slug the job runs with.
    pub model: String,
}
