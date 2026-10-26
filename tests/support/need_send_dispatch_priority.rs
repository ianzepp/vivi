//! Dispatch priority coverage lands here: `--priority` is written only
//! after the record is minted, keyed by the record's own need (never the
//! sent handle or the record handle), level 0 keeps the zero-clearing
//! semantics of `vivi pump priority set`, a dispatch without `--priority`
//! never creates `pump.db`, and a failed priority write leaves the filed
//! need and the open record untouched.
//!
//! The tests in this module reach the fixtures of the parent integration
//! test (`roster`, `vivi`, `ok`, `records`, `handle_after`, ...) through
//! `super::*`.
