//! Admission control: the in-flight gate a pool applies before handing out
//! a slot to a job.
//!
//! Shell only. The gate itself and its health view arrive with later
//! slices; nothing here pretends to admit or health-check anything yet.
