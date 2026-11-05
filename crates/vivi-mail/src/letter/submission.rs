//! Serializable, versioned context supplied with a role reply.

pub mod context;
pub use context::*;

#[cfg(test)]
#[path = "submission_test.rs"]
mod tests;
