//! Dispatch identity coverage lands here: the minted record keeps the
//! ordinary `pump send` sender and recipient — the configured `[pump] from`
//! and `[pump] reply_from`, by name, alias or local address — across Cc
//! copies and the delivered-copy resolution the dispatch requires, and never
//! inherits the need sender.
//!
//! The tests in this module reach the fixtures of the parent integration
//! test (`roster`, `vivi`, `ok`, `records`, `handle_after`, ...) through
//! `super::*`.
