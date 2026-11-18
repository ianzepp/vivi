//! Errors raised by the spike's own code (not by Temporal).

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SpikeError {
    #[error("bad fault script entry {entry:?}: {reason}")]
    FaultScript { entry: String, reason: String },
    #[error("stand-in state lock poisoned: {0}")]
    Poisoned(String),
    #[error("no scenario for need {0:?}")]
    UnknownNeed(String),
    #[error("no slice {slice:?} in need {need:?}")]
    UnknownSlice { need: String, slice: String },
    #[error("push to {branch} refused: expected head {expected:?}, found {found:?}")]
    PushRejected {
        branch: String,
        expected: Option<String>,
        found: Option<String>,
    },
}

impl<T> From<std::sync::PoisonError<T>> for SpikeError {
    fn from(err: std::sync::PoisonError<T>) -> Self {
        SpikeError::Poisoned(err.to_string())
    }
}
