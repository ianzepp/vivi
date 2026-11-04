//! Tests for [`super::slots`] and [`super::parse_created_handle`].

use std::sync::Arc;

use tempfile::tempdir;

use super::{parse_created_handle, slots};
use crate::pump::config::PumpConfig;
use crate::pump::host::{Ctx, FakeHost, test_ctx};

/// A context over a fresh temp dir with the default config.
fn setup() -> (tempfile::TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    (dir, ctx, host)
}

#[test]
fn parse_created_handle_reads_node_and_created_lines_and_ignores_sent() {
    let out = "created hand 1a2b3c4d\nnode 1a2b3c4d\nsent 5e6f7a8b";
    assert_eq!(parse_created_handle(out).as_deref(), Some("1a2b3c4d"));
    assert_eq!(
        parse_created_handle("sent 5e6f7a8b\nnode beef1234\n").as_deref(),
        Some("beef1234")
    );
    assert_eq!(
        parse_created_handle("created 1a2b3c4d").as_deref(),
        Some("1a2b3c4d")
    );
    assert_eq!(parse_created_handle("created 1a2b3c4"), None);
    assert_eq!(parse_created_handle("node ABCD1234"), None);
    assert_eq!(parse_created_handle("node 1a2b3c4d extra 12345678"), None);
    assert_eq!(parse_created_handle("sent 1a2b3c4d"), None);
    assert_eq!(parse_created_handle("createdx 1a2b3c4d"), None);
    assert_eq!(parse_created_handle(""), None);
}

#[test]
fn slots_counts_free_slots_per_pool() {
    let (_dir, ctx, host) = setup();
    host.reply(
        "pool status",
        0,
        "slot fly4 bridge slot-1 free\nslot fly4 bridge slot-2 busy\n\
         slot fly4 bridge slot-3 free\nslot flyfull bridge slot-9 free\nnoise line here",
    );
    let free = slots(&ctx);
    assert_eq!(free.get("fly4"), Some(&2));
    assert_eq!(free.get("flyfull"), Some(&1));
    assert_eq!(free.len(), 2);
    assert!(
        host.commands()
            .iter()
            .any(|cmd| cmd.contains("pool status --porcelain"))
    );
}

#[test]
fn slots_counts_the_capacity_of_per_dispatch_pools() {
    let (_dir, ctx, host) = setup();
    host.reply(
        "pool status",
        0,
        "slot studio ms-studio-slot-1 ms/studio/slot-1 busy t1 - - - - 2026-10-21T20:00:00Z write\n\
         capacity studio 1\ncapacity gpu 0\ncapacity runpod 3\n\
         slot fly4 bridge slot-1 free\ncapacity broken x",
    );
    let free = slots(&ctx);
    assert_eq!(
        free.get("studio"),
        Some(&1),
        "a worker pool with one live pod of two"
    );
    assert_eq!(free.get("runpod"), Some(&3));
    assert_eq!(
        free.get("fly4"),
        Some(&1),
        "resting pools still count free slots"
    );
    assert_eq!(
        free.get("gpu"),
        None,
        "a pool with no capacity left is full"
    );
    assert_eq!(free.get("broken"), None, "an unreadable count is ignored");
}
