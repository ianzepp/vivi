use crate::sync::fake_server::{World, handle, letter, wire_id};
use crate::sync::pull::{PullPlan, PullReport, pull_with};
use crate::sync::refs::{MAX_DEPTH, MAX_LOOKUPS, RefsReport, follow_refs, references_in};

fn eml(subject: &str, body: &str, extra: &str) -> Vec<u8> {
    letter(
        "msg_00000000000000000000000f",
        "mind@a1b2c3d4.local",
        "mind@e5f6a7b8.local",
        subject,
        body,
        "Sun, 11 Oct 2026 12:00:00 +0000",
        extra,
    )
}

#[test]
fn references_come_from_the_header_the_subject_and_the_body_in_order() {
    let bytes = eml(
        "Re: 0000AAAA status",
        "see 000000b2 and (000000c3), again 000000b2.\r\nand 000000D4\r\n",
        "X-Vivi-Depends-On: msg_00000e0e00000000000000f0\r\n",
    );

    assert_eq!(
        references_in(&bytes),
        vec!["00000e0e", "0000aaaa", "000000b2", "000000c3", "000000d4"]
    );
}

#[test]
fn only_whole_word_eight_hex_tokens_count() {
    let sha = "0123456789abcdef0123456789abcdef01234567";
    let body = format!(
        "commit {sha}\r\nlonger 000000b2c3 and 00b2\r\nsnake_000000b2 and 000000g2\r\n\
         words: deadbeef, facade\r\n"
    );

    let found = references_in(&eml("plain subject", &body, ""));

    assert_eq!(found, vec!["deadbeef"]);
}

#[test]
fn a_depends_on_header_that_is_not_a_handle_is_ignored() {
    for value in ["not-a-handle", "12345", "zzzzzzzz", ""] {
        let extra = format!("X-Vivi-Depends-On: {value}\r\n");
        assert!(
            references_in(&eml("s", "b", &extra)).is_empty(),
            "{value:?}"
        );
    }
}

/// Bob's window starts after `old`; `new` is the letter the window holds.
fn windowed_world(chain: &[(u64, &str, &str)]) -> (World, PullReport) {
    let w = World::new();
    for (n, body, header) in chain {
        w.at("2026-09-01T00:00:00Z");
        if *n >= 0xc0 {
            w.at("2026-10-10T00:00:00Z");
        }
        w.put_full(
            &wire_id(*n, 0),
            "need",
            &w.bob.address("mind"),
            &format!("letter {n:x}"),
            body,
            "Sun, 11 Oct 2026 12:00:00 +0000",
            header,
        );
    }
    let plan = PullPlan {
        window: Some("2026-10-05T00:00:00Z"),
        resync: false,
    };
    let report = pull_with(
        &w.bob.mailspace,
        &w.bob.settings,
        &w.bob_client,
        &plan,
        &mut |_| {},
    )
    .unwrap();
    (w, report)
}

fn follow(w: &World, report: &mut PullReport, depth: u8) -> RefsReport {
    follow_refs(
        &w.bob.mailspace,
        &w.bob.settings,
        &w.bob_client,
        report,
        depth,
    )
    .unwrap()
}

/// a1 <- b2 (body names a1) <- c3 (header names b2). Only c3 is in the window.
fn chain() -> Vec<(u64, &'static str, &'static str)> {
    vec![
        (0xa1, "the root", ""),
        (0xb2, "builds on 000000a1", ""),
        (0xc3, "the head", "X-Vivi-Depends-On: 000000b2\r\n"),
    ]
}

#[test]
fn depth_one_follows_one_hop() {
    let (w, mut report) = windowed_world(&chain());
    assert_eq!(w.live_ids(), vec![wire_id(0xc3, 0)]);
    let state = w.state();

    let refs = follow(&w, &mut report, 1);

    assert_eq!(
        (refs.hops, refs.found, refs.looked_up, refs.filed),
        (1, 1, 1, 1)
    );
    assert_eq!(w.live_ids(), vec![wire_id(0xb2, 0), wire_id(0xc3, 0)]);
    assert_eq!(w.server.lookups(), vec![1]);
    assert_eq!(w.state(), state, "a follow moves neither cursor nor floor");
    assert!(
        refs.summary().starts_with("sync refs: 1 hop(s)"),
        "{}",
        refs.summary()
    );
}

#[test]
fn depth_two_follows_what_the_first_hop_filed() {
    let (w, mut report) = windowed_world(&chain());

    let refs = follow(&w, &mut report, 2);

    assert_eq!((refs.hops, refs.looked_up, refs.filed), (2, 2, 2));
    assert_eq!(
        w.live_ids(),
        vec![wire_id(0xa1, 0), wire_id(0xb2, 0), wire_id(0xc3, 0)]
    );
    assert_eq!(w.server.lookups(), vec![1, 1]);
}

#[test]
fn held_letters_and_repeated_references_are_not_looked_up_again() {
    let (w, mut report) = windowed_world(&chain());
    follow(&w, &mut report, 3);
    assert_eq!(w.live_ids().len(), 3);
    let lookups = w.server.lookups().len();

    // A second follow over the same report finds everything held.
    let again = follow(&w, &mut report, 3);

    assert_eq!((again.found, again.looked_up), (0, 0));
    assert_eq!(w.server.lookups().len(), lookups);
}

#[test]
fn tokens_that_match_nothing_are_counted_and_ignored() {
    let sha = "0123456789abcdef0123456789abcdef01234567";
    let body = format!("fixed in deadbeef (commit {sha}) per 000000b2c3d4");
    let (w, mut report) = windowed_world(&[(0xc3, &body, "")]);

    let refs = follow(&w, &mut report, 3);

    assert_eq!(
        (refs.found, refs.looked_up, refs.filed, refs.unmatched),
        (1, 1, 0, 1)
    );
    assert_eq!(
        w.server.lookups(),
        vec![1],
        "only the 8-hex word was asked for"
    );
    assert_eq!(w.live_ids(), vec![wire_id(0xc3, 0)]);
}

#[test]
fn an_invocation_looks_up_at_most_five_hundred_handles() {
    let tokens: Vec<String> = (0..600u64).map(|n| handle(0x1000_0000 + n)).collect();
    let body = tokens.join(" ");
    let (w, mut report) = windowed_world(&[(0xc3, &body, "")]);

    let refs = follow(&w, &mut report, 3);

    assert_eq!(MAX_LOOKUPS, 500);
    assert_eq!(refs.found, 600);
    assert_eq!(refs.looked_up, 500);
    assert_eq!(refs.capped, 100);
    assert_eq!(w.server.lookups(), vec![100; 5]);
}

#[test]
fn depth_outside_one_to_three_is_refused() {
    let (w, mut report) = windowed_world(&chain());
    for depth in [0, MAX_DEPTH + 1] {
        let error = follow_refs(
            &w.bob.mailspace,
            &w.bob.settings,
            &w.bob_client,
            &mut report,
            depth,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("--depth must be 1 to 3"), "{error}");
    }
    assert!(w.server.lookups().is_empty());
}

#[test]
fn a_reply_found_through_a_reference_links_to_its_parent() {
    let w = World::new();
    let to = w.bob.address("mind");
    let parent = wire_id(0xa1, 0);
    w.at("2026-09-01T00:00:00Z");
    w.put_full(
        &parent,
        "need",
        &to,
        "parent",
        "root",
        "Sun, 11 Oct 2026 12:00:00 +0000",
        "",
    );
    w.at("2026-10-10T00:00:00Z");
    let header = format!("In-Reply-To: <{parent}@x.test>\r\n");
    w.put_full(
        &wire_id(0xb2, 0),
        "need",
        &to,
        "child",
        "re 000000a1",
        "Sun, 11 Oct 2026 12:00:00 +0000",
        &header,
    );
    let plan = PullPlan {
        window: Some("2026-10-05T00:00:00Z"),
        resync: false,
    };
    let mut report = pull_with(
        &w.bob.mailspace,
        &w.bob.settings,
        &w.bob_client,
        &plan,
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(w.state().dangling.len(), 1, "the parent is not held yet");

    let refs = follow(&w, &mut report, 1);

    assert_eq!(refs.filed, 1);
    assert_eq!(report.linked, 1);
    assert!(w.state().dangling.is_empty());
}

#[test]
fn a_format_one_letters_front_matter_depends_on_is_a_reference() {
    let bytes = eml(
        "work",
        "+++\ndepends_on = [\"msg_00000e0e00000000000000f0\"]\n+++\nprose\r\n",
        "X-Vivi-Format: 1\r\nX-Vivi-Kind: task\r\n",
    );

    assert_eq!(references_in(&bytes), vec!["00000e0e"]);
}
