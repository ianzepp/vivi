//! Purging an archived project: once [`PURGE_DELAY_SECONDS`](super::PURGE_DELAY_SECONDS)
//! have passed since the archive stamp, remove the project's mailspace root
//! and lanes directory for good. The archive record stays on the retired
//! metadata so the name remains taken and the archive is still remembered.
//! Implemented by its own slice on this foundation; nothing here yet but
//! this documentation.
