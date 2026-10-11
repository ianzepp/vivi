use crate::sync::fake_server::{LETTER_DATE, World, handle, wire_id};
use crate::sync::fetch::{FetchReport, fetch_handles, normalize_handles};
use crate::sync::sync_dir;

fn fetch(w: &World, handles: &[&str]) -> Result<FetchReport, crate::VivariumError> {
    let handles: Vec<String> = handles.iter().map(|h| (*h).to_string()).collect();
    fetch_handles(&w.bob.mailspace, &w.bob.settings, &w.bob_client, &handles)
}

#[test]
fn a_held_by_the_server_letter_is_filed_byte_for_byte_and_the_cursor_stays() {
    let w = World::new();
    let id = w.put(0xa1, "wanted", "the body");

    let report = fetch(&w, &["000000A1"]).unwrap();

    assert_eq!(report.handles, 1);
    assert_eq!(report.pull.filed.values().sum::<usize>(), 1);
    assert!(report.missing.is_empty());
    assert!(report.failure().is_none());
    let row = w.bob.row(&id).expect("filed under its wire id");
    assert_eq!(row.handle, "000000a1");
    assert_eq!(w.bob.bytes_of("000000a1"), w.server.items()[0].bytes);
    assert_eq!(w.bob.blob_hash(&row), w.server.items()[0].sha);
    assert!(
        !sync_dir(&w.bob.mailspace.dir).join("state.json").exists(),
        "a fetch writes no cursor and no floor"
    );
    assert_eq!(w.state().since, 0);
    assert_eq!(w.state().floor, None);
    assert_eq!(w.server.lists().len(), 0, "no list request was made");

    // The pull that comes later finds the letter held, and still starts at 0.
    let pulled = w.bob.pull(&w.bob_client);
    assert_eq!((pulled.fetched, pulled.existing, pulled.cursor), (1, 1, 1));
}

#[test]
fn a_handle_that_matches_nothing_is_missing_not_a_failure() {
    let w = World::new();
    w.put(0xa1, "wanted", "x");

    let report = fetch(&w, &["000000a1", "deadbeef"]).unwrap();

    assert_eq!(report.missing, vec!["deadbeef".to_string()]);
    assert_eq!(report.pull.filed.values().sum::<usize>(), 1);
    assert!(report.failure().is_none());
    let line = report.summary();
    assert!(line.contains("1 missing (deadbeef)"), "{line}");
    assert_eq!(w.server.lookups(), vec![2]);
}

#[test]
fn mail_the_caller_may_not_see_is_missing() {
    let w = World::new();
    let carol = "mind@c0ffee00.local";
    w.put_full(
        &wire_id(0xb2, 0),
        "mail",
        carol,
        "private",
        "x",
        LETTER_DATE,
        "",
    );

    let report = fetch(&w, &["000000b2"]).unwrap();

    assert_eq!(report.missing, vec!["000000b2".to_string()]);
    assert!(w.live_ids().is_empty());
}

#[test]
fn a_handle_that_names_two_letters_files_both() {
    let w = World::new();
    let to = w.bob.address("mind");
    for tail in [1, 2] {
        let id = wire_id(0xc3, tail);
        w.put_full(
            &id,
            "need",
            &to,
            &format!("twin {tail}"),
            "x",
            LETTER_DATE,
            "",
        );
    }

    let report = fetch(&w, &["000000c3"]).unwrap();

    assert!(report.missing.is_empty());
    assert_eq!(report.pull.filed.values().sum::<usize>(), 2);
    assert_eq!(w.live_ids(), vec![wire_id(0xc3, 1), wire_id(0xc3, 2)]);
}

#[test]
fn a_record_that_fails_verification_is_refused_and_fails_the_command() {
    let w = World::new();
    let id = w.put(0xd4, "tampered", "x");
    w.server.tamper(&id, b"not the letter");

    let report = fetch(&w, &["000000d4"]).unwrap();

    assert_eq!(report.refused().len(), 1);
    assert!(report.failure().is_some());
    assert!(w.live_ids().is_empty());
}

#[test]
fn handles_go_to_the_server_a_hundred_at_a_time() {
    let w = World::new();
    let all: Vec<String> = (0..150u64).map(handle).collect();
    let refs: Vec<&str> = all.iter().map(String::as_str).collect();

    let report = fetch(&w, &refs).unwrap();

    assert_eq!(report.handles, 150);
    assert_eq!(report.missing.len(), 150);
    assert_eq!(w.server.lookups(), vec![100, 50]);
}

#[test]
fn values_that_are_not_handles_are_refused_before_any_request() {
    let w = World::new();
    for bad in ["xyz", "0000a1", "000000a1f", "msg_000000a1", "0000 0a1"] {
        let error = fetch(&w, &[bad]).unwrap_err().to_string();
        assert!(error.contains("not a handle"), "{bad}: {error}");
    }
    assert!(w.server.lookups().is_empty());
    assert_eq!(
        normalize_handles(&["000000A1".into(), " 000000a1 ".into(), "000000b2".into()]).unwrap(),
        vec!["000000a1".to_string(), "000000b2".to_string()],
        "case-folded and de-duplicated, order kept"
    );
}

#[test]
fn a_reply_fetched_before_its_parent_links_once_the_parent_arrives() {
    let w = World::new();
    let to = w.bob.address("mind");
    let parent = wire_id(0xa1, 0);
    w.put_full(&parent, "need", &to, "parent", "x", LETTER_DATE, "");
    let reply = wire_id(0xa2, 0);
    let header = format!("In-Reply-To: <{parent}@x.test>\r\n");
    w.put_full(&reply, "need", &to, "child", "x", LETTER_DATE, &header);

    let first = fetch(&w, &["000000a2"]).unwrap();
    assert_eq!(first.pull.linked, 0, "the parent is not here yet");
    assert_eq!(w.state().dangling.len(), 1);

    let second = fetch(&w, &["000000a1"]).unwrap();
    assert_eq!(second.pull.linked, 1);
    assert!(w.state().dangling.is_empty());
}
