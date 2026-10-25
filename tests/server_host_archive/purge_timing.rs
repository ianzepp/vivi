//! Purge timing cases (`tests/host_archive.rs` child module): purge is
//! only allowed once the archive has aged past its 24-hour delay, judged
//! by the fixture clock (`Fx::clock`, `Arc<AtomicI64>`) — moving the
//! atomic forward simulates elapsed time, and no archive timestamp in
//! metadata is ever altered by hand. Arrives with the purge tool's public
//! wiring; nothing is implemented here yet.
