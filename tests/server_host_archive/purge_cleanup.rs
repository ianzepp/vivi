//! Purge cleanup cases (`tests/host_archive.rs` child module): an aged
//! archived project is purged for good — the mailspace and lanes trees go
//! away (compared with `snapshot` before and after), the metadata row and
//! registry entry are dropped, and the refusal modes (not archived, delay
//! not elapsed, pump lock held) leave nothing behind. Arrives with the
//! purge tool's public wiring; nothing is implemented here yet.
