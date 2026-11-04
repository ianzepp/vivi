//! Secure directory creation for the mailspace store.
//!
//! The maildir and blob layer lives in `vivi-mail`; the mailspace side needs
//! only the permission-restricted directory constructor.

mod secure;

pub use secure::secure_create_dir_all;
