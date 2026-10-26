//! Error type for the mailspace crate.

use std::io;

#[derive(Debug, thiserror::Error)]
pub enum VivariumError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error(transparent)]
    Io(#[from] io::Error),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("message error: {0}")]
    Message(String),

    /// A reserved built-in identity was used where only a configured role may
    /// appear. Reserved identities resolve without a roster row and can never
    /// be created, renamed, edited, or given a charter.
    #[error("'{0}' is a reserved built-in identity and cannot be used as a configurable role")]
    ReservedIdentity(String),

    #[error("{0}")]
    Other(String),
}
