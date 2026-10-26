//! Dispatch refusal coverage lands here: a `--dispatch` send without a
//! `[pump]` table, with an unresolvable `[pump] reply_from`, or without a
//! delivered copy of the filed need for the pump fails after the need is
//! filed, wraps the failure in the dispatch boundary wording (naming the
//! sent handle), and never writes a priority row.
//!
//! The tests in this module reach the fixtures of the parent integration
//! test (`roster`, `vivi`, `ok`, `records`, `handle_after`,
//! `remove_starter_pump`, ...) through `super::*`.
