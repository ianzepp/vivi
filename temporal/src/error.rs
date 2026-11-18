//! Errors raised by the spike's own code (not by Temporal).

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SpikeError {
    #[error("bad fault script entry {entry:?}: {reason}")]
    FaultScript { entry: String, reason: String },
    #[error("record: {0}")]
    Record(String),
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

/// A worker config that cannot be used.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("config is not valid TOML: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("config: {0}")]
    Invalid(String),
}

/// A command a real activity runs could not be started or watched.
#[derive(Debug, Error)]
pub enum RunError {
    #[error("cannot start {program}: {source}")]
    Spawn {
        program: String,
        source: std::io::Error,
    },
    #[error("{program}: {source}")]
    Io {
        program: String,
        source: std::io::Error,
    },
    #[error("{0}")]
    Config(#[from] ConfigError),
}
