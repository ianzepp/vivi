#![deny(clippy::pedantic)]

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

    #[error("{0}")]
    Other(String),
}
