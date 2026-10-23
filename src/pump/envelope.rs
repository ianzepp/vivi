//! The reply envelope: Markdown with TOML front matter between `+++` fences.
//!
//! Every role reply opens with the envelope. The front matter carries the
//! typed `answer` and its fields; the prose report is the body.
//! The parser lives in `crate::letter`.

pub mod correct;
pub mod review;
pub mod roles;

pub use crate::letter::{Envelope, LetterError as EnvelopeError, Validate, parse, split};

#[cfg(test)]
#[path = "envelope_test.rs"]
mod tests;
