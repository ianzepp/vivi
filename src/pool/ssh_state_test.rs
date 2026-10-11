use super::*;
use crate::pool::slot::{META_BASE, META_POOL, META_STATE, slot_state};

const LISTING: &str = "\
@@slot lanes/slot-1
state=stopped
exit=0
created=2026-10-11T12:00:00Z
mark.vivi_pool=lanes
mark.vivi_pool_slot=1
mark.vivi_pool_base=main
mark.vivi_pool_state=ready
mark.vivi_pool_arch=amd64
@@slot lanes/slot-2
state=started
created=2026-10-11T12:01:00Z
mark.vivi_pool=lanes
mark.vivi_pool_slot=2
mark.vivi_pool_state=ready
claim.task=task-abc
claim.claimed=1700000000
";

#[test]
fn a_listing_becomes_records_with_marks_and_the_claim_as_metadata() {
    let records = parse_listing(LISTING, "pharos").unwrap();
    assert_eq!(records.len(), 2);
    let one = &records[0];
    assert_eq!(
        (one.id.as_str(), one.name.as_str()),
        ("lanes/slot-1", "lanes-slot-1")
    );
    assert_eq!(one.state, "stopped");
    assert_eq!(one.exit_code, Some(0));
    assert_eq!(one.region, "pharos");
    assert_eq!(one.created_at, "2026-10-11T12:00:00Z");
    assert_eq!(one.metadata[META_POOL], "lanes");
    assert_eq!(one.metadata[META_BASE], "main");
    assert_eq!(one.metadata[META_STATE], "ready");
    assert_eq!(slot_state(one), "free");
    let two = &records[1];
    assert_eq!(two.state, "started");
    assert_eq!(two.exit_code, None);
    assert_eq!(two.metadata["vivi_pool_task"], "task-abc");
    assert_eq!(two.metadata["vivi_pool_claimed"], "1700000000");
    assert_eq!(slot_state(two), "busy");
}

#[test]
fn a_claimed_stopped_slot_reads_finished_uncollected_and_a_new_one_unseeded() {
    let text = "@@slot p/slot-3\nstate=stopped\nmark.vivi_pool=p\nmark.vivi_pool_state=ready\nclaim.task=t\n@@slot p/slot-4\nstate=stopped\nexit=4\nmark.vivi_pool=p\nmark.vivi_pool_state=seeding\n";
    let records = parse_listing(text, "h").unwrap();
    assert_eq!(slot_state(&records[0]), "finished-uncollected");
    assert_eq!(slot_state(&records[1]), "unseeded");
    assert_eq!(records[1].exit_code, Some(4));
}

#[test]
fn an_empty_listing_and_noise_between_blocks_are_fine_and_a_stray_value_is_not() {
    assert!(parse_listing("", "h").unwrap().is_empty());
    assert!(parse_listing("login banner\n", "h").unwrap().is_empty());
    assert!(parse_listing("state=started\n", "h").is_err());
}

#[test]
fn a_mark_with_no_value_is_no_mark() {
    let text = "@@slot p/slot-1\nstate=stopped\nmark.vivi_pool=p\nmark.vivi_pool_base=\n";
    let records = parse_listing(text, "h").unwrap();
    assert!(!records[0].metadata.contains_key("vivi_pool_base"));
}

#[test]
fn slot_ids_are_pool_slash_slot_number_and_nothing_else() {
    assert_eq!(slot_id("lanes", "3"), "lanes/slot-3");
    assert_eq!(name_of("lanes/slot-3"), "lanes-slot-3");
    for good in ["a/slot-1", "my-pool-2/slot-12", "9/slot-0"] {
        assert!(is_slot_id(good), "{good}");
    }
    for bad in [
        "",
        "slot-1",
        "a/slot-",
        "a/slot-x",
        "a/slot-1/",
        "a/slot-1/x",
        "../slot-1",
        "A/slot-1",
        "-a/slot-1",
        "a b/slot-1",
        "/slot-1",
        "a/../slot-1",
        "a/slot-1;rm",
        "a//slot-1",
        "a/Slot-1",
        "a/slot-1\n",
    ] {
        assert!(!is_slot_id(bad), "{bad:?}");
    }
}
