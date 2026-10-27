//! The server-only incremental index path: discovery, selection, send,
//! and state in one pass.
//!
//! The live pass is still `push::push` in the parent module, and the scope
//! ruling holds: "The generic CLI sync implementation is not changed."
