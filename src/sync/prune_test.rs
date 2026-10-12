use chrono::{DateTime, TimeZone, Utc};

use crate::sync::fake_server::{World, handle, wire_id};
use crate::sync::fetch::fetch_handles;
use crate::sync::prune::{PruneReport, prune, report_lines};
use crate::sync::pull;
use crate::sync::refs::follow_refs;
use crate::sync::window::parse_age;

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2100, 1, 1, 0, 0, 0).unwrap()
}

fn thirty_days() -> std::time::Duration {
    parse_age("30d").unwrap()
}

/// Six pulled letters and one local one:
/// 1 and 5 are done (evictable), 2 is starred, 3 is absorbed, 4 is an open
/// need, 6 is recent, and `local` is a letter that never travelled.
struct Fixture {
    w: World,
    local_id: String,
}

fn fixture() -> Fixture {
    let w = World::new();
    for n in 1..=5 {
        w.put(n, &format!("letter {n}"), "body");
    }
    w.put_dated(6, "recent", "body", "25 Dec 2099 12:00:00 +0000");
    w.bob.pull(&w.bob_client);
    let local = w
        .bob
        .send_need("mind", &w.bob.address("hand"), "local only");
    let local_id = w.bob.id_of(&local);
    {
        let mut storage = w.bob.mailspace.storage().unwrap();
        for n in [1, 2, 3, 5, 6] {
            storage
                .move_message_to_role("mind", &wire_id(n, 0), "done")
                .unwrap();
        }
        storage
            .move_message_to_role("hand", &local_id, "done")
            .unwrap();
        storage
            .absorb_message("mind", &wire_id(3, 0), "test")
            .unwrap();
        storage
            .update_remote_flags("mind", &w.bob.settings.project, 1, 2, false, true)
            .unwrap();
    }
    Fixture { w, local_id }
}

fn handles(report: &PruneReport) -> Vec<String> {
    let mut found: Vec<String> = report.items.iter().map(|i| i.handle.clone()).collect();
    found.sort();
    found
}

#[test]
fn only_old_unbound_free_closed_letters_are_selected_and_a_dry_run_changes_nothing() {
    let Fixture { w, local_id } = fixture();
    let before = w.bob.rows();

    let dry = prune(&w.bob.mailspace, thirty_days(), now(), true).unwrap();

    assert_eq!(handles(&dry), vec![handle(1), handle(5)]);
    assert!(dry.dry_run);
    assert_eq!(dry.evicted, 0);
    assert_eq!(w.bob.rows(), before, "a dry run changes nothing");
    let lines = report_lines(&dry);
    assert!(lines[0].starts_with("would evict 2 letter(s)"), "{lines:?}");
    assert!(lines.last().unwrap().contains("dry run"), "{lines:?}");

    let real = prune(&w.bob.mailspace, thirty_days(), now(), false).unwrap();

    assert_eq!(
        real.items, dry.items,
        "a dry run previews the real selection"
    );
    assert_eq!(real.evicted, 2);
    let live = w.live_ids();
    for kept in [2, 3, 4, 6] {
        assert!(live.contains(&wire_id(kept, 0)), "letter {kept} is kept");
    }
    for gone in [1, 5] {
        assert!(
            !live.contains(&wire_id(gone, 0)),
            "letter {gone} is evicted"
        );
    }
    assert!(
        w.bob.row(&local_id).is_some(),
        "a local-origin letter is never evicted"
    );
    assert_eq!(
        prune(&w.bob.mailspace, thirty_days(), now(), false)
            .unwrap()
            .evicted,
        0,
        "pruning twice evicts nothing the second time"
    );
}

#[test]
fn fetch_and_resync_bring_evicted_letters_back_byte_for_byte_with_local_state() {
    let Fixture { w, .. } = fixture();
    let snapshot = |n: u64| {
        let id = wire_id(n, 0);
        (w.bob.row(&id).unwrap(), w.bob.bytes_of(&handle(n)))
    };
    let (view_1, bytes_1) = snapshot(1);
    let (view_5, bytes_5) = snapshot(5);
    prune(&w.bob.mailspace, thirty_days(), now(), false).unwrap();
    assert!(!w.bob.mailspace.dir.join(&view_1.blob_relpath).exists());
    let state_before = w.state();

    // A plain pull never brings them back: the cursor is past them.
    let plain = w.bob.pull(&w.bob_client);
    assert_eq!((plain.fetched, plain.restored), (0, 0));

    let fetched = fetch_handles(
        &w.bob.mailspace,
        &w.bob.settings,
        &w.bob_client,
        &[handle(1)],
    )
    .unwrap();
    assert_eq!(fetched.pull.restored, 1);
    assert_eq!(
        w.state(),
        state_before,
        "a fetch moves neither cursor nor floor"
    );
    let back = w.bob.row(&wire_id(1, 0)).unwrap();
    assert_eq!(back.content_id, view_1.content_id);
    assert_eq!(back.local_role, "done", "local state survives");
    assert_eq!(w.bob.bytes_of(&handle(1)), bytes_1);

    let plan = pull::PullPlan {
        window: None,
        resync: true,
    };
    let resync = pull::pull_with(
        &w.bob.mailspace,
        &w.bob.settings,
        &w.bob_client,
        &plan,
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(resync.restored, 1, "only letter 5 was still evicted");
    assert_eq!(resync.existing, 5);
    let back = w.bob.row(&wire_id(5, 0)).unwrap();
    assert_eq!(back.content_id, view_5.content_id);
    assert_eq!(back.local_role, "done");
    assert_eq!(w.bob.bytes_of(&handle(5)), bytes_5);
    assert_eq!(w.bob.blob_hash(&back), w.server.items()[4].sha);
}

#[test]
fn a_reference_to_an_evicted_letter_restores_it() {
    let Fixture { w, .. } = fixture();
    prune(&w.bob.mailspace, thirty_days(), now(), false).unwrap();
    let body = format!("see {} for the history", handle(1));
    w.put(7, "points back", &body);

    let mut report = w.bob.pull(&w.bob_client);
    let refs = follow_refs(
        &w.bob.mailspace,
        &w.bob.settings,
        &w.bob_client,
        &mut report,
        1,
    )
    .unwrap();

    assert_eq!((refs.found, refs.looked_up, refs.filed), (1, 1, 1));
    assert_eq!(report.restored, 1);
    assert!(w.live_ids().contains(&wire_id(1, 0)));
    assert_eq!(w.server.lookups(), vec![1]);
}

#[test]
fn prune_is_scoped_to_the_configured_project() {
    let Fixture { w, .. } = fixture();
    let mut other = w.bob.mailspace.clone();
    other.config.sync.as_mut().unwrap().project = Some("bcdef-ghjkm-npqrs-tvwxy-z2345".into());

    let report = prune(&other, thirty_days(), now(), true).unwrap();

    assert!(report.items.is_empty(), "letters of another project stay");
}

#[test]
fn the_sent_copy_of_open_work_is_kept_until_it_closes() {
    let w = World::new();
    let away = "hand@a1b2c3d4.local";
    let sent = w.bob.send_need("mind", away, "away");
    let local = w
        .bob
        .send_need("mind", &w.bob.address("hand"), "stays home");
    w.bob.push(&w.bob_client);

    let open = prune(&w.bob.mailspace, thirty_days(), now(), true).unwrap();

    assert!(
        handles(&open).is_empty(),
        "an open sent copy is the item in flight: {:?}",
        open.items
    );
    assert_eq!(w.bob.row(&w.bob.id_of(&sent)).unwrap().local_role, "sent");

    w.bob
        .mailspace
        .move_item("mind", &sent, "done", None, "need done", None)
        .unwrap();
    let closed = prune(&w.bob.mailspace, thirty_days(), now(), true).unwrap();

    assert_eq!(handles(&closed), vec![sent], "closed work evicts as before");
    assert!(w.bob.row(&w.bob.id_of(&local)).is_some());
}

#[test]
fn the_cutoff_is_the_letter_date_not_when_it_was_pulled() {
    let w = World::new();
    w.put_dated(1, "before", "body", "08 Sep 2099 23:59:59 +0000");
    w.put_dated(2, "at", "body", "09 Sep 2099 00:00:00 +0000");
    w.put_dated(3, "after", "body", "09 Sep 2099 00:00:01 +0000");
    w.bob.pull(&w.bob_client);
    {
        let mut storage = w.bob.mailspace.storage().unwrap();
        for n in 1..=3 {
            storage
                .move_message_to_role("mind", &wire_id(n, 0), "done")
                .unwrap();
        }
    }
    let now = Utc.with_ymd_and_hms(2099, 10, 9, 0, 0, 0).unwrap();

    let report = prune(&w.bob.mailspace, thirty_days(), now, false).unwrap();

    assert_eq!(report.cutoff, "2099-09-09T00:00:00+00:00");
    assert_eq!(
        handles(&report),
        vec![handle(1)],
        "strictly older than the cutoff"
    );
}
