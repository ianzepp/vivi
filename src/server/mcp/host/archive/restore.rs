//! Restoring an archived project: moving its mailspace root and lanes
//! directory back where the host expects them, clearing the archive record
//! from the project's metadata, and re-registering the project so the pump
//! and the host registry serve it again (its name becomes live once more).
//! Implemented by its own slice on this foundation; nothing here yet but
//! this documentation.
