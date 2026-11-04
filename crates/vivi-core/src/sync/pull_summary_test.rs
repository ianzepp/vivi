//! Pull summary accounting: sequence numbers the server's cursor jumps
//! over, and replies whose parent letter is not held.

use crate::sync::fake_server::{LETTER_DATE, World, wire_id};

/// The server shows carol only what she sent, what is addressed to her, and
/// work items, and its cursor jumps over the rest: the summary has to say
/// where every moved sequence number went.
#[test]
fn the_summary_accounts_for_sequence_numbers_the_server_hides() {
    let w = World::new();
    let carol = crate::sync::fake_server::checkout("carol", &["mind"]);
    let to = w.bob.address("mind");
    w.put(1, "need-one", "x");
    w.put_full(
        &wire_id(11, 0),
        "mail",
        &to,
        "mail-one",
        "x",
        LETTER_DATE,
        "",
    );
    w.put(2, "need-two", "x");
    w.put_full(
        &wire_id(12, 0),
        "mail",
        &to,
        "mail-two",
        "x",
        LETTER_DATE,
        "",
    );
    w.put(3, "need-three", "x");
    w.put_full(
        &wire_id(13, 0),
        "mail",
        &to,
        "mail-three",
        "x",
        LETTER_DATE,
        "",
    );
    w.server.skip_hidden(true);

    let report = carol.pull(&carol.client(&w.server));

    assert_eq!((report.fetched, report.cursor), (3, 6));
    assert_eq!(report.cursor_before, 0);
    assert_eq!(report.skipped_by_server, 3);
    assert!(
        report.summary().contains(
            "cursor 6 (moved 6: 3 shown, 3 not shown to this member: \
             mail addressed to other mailspaces)",
        ),
        "{}",
        report.summary()
    );

    let again = carol.pull(&carol.client(&w.server));
    assert_eq!((again.fetched, again.cursor), (0, 6));
    assert_eq!(again.skipped_by_server, 0, "a still cursor skips nothing");
    assert!(again.summary().ends_with("cursor 6"), "{}", again.summary());
}

/// A reply is filed even when its parent letter is not held; the summary
/// counts it so `vivi sync pull --refs` can find the missing parent.
#[test]
fn a_reply_whose_parent_is_not_held_is_counted_unlinked() {
    let w = World::new();
    w.put_full(
        &wire_id(21, 0),
        "mail",
        &w.bob.address("mind"),
        "Re: ghost",
        "x",
        LETTER_DATE,
        "In-Reply-To: <ghost@x.test>",
    );

    let report = w.bob.pull(&w.bob_client);

    assert_eq!(report.filed.get("inbox"), Some(&1));
    assert_eq!((report.linked, report.unlinked), (0, 1));
    assert!(
        report
            .summary()
            .contains(", 1 repl(ies) to letters not held here (vivi sync pull --refs finds them)",),
        "{}",
        report.summary()
    );
    assert_eq!(
        w.state().dangling.len(),
        1,
        "the link is retried on the next pull"
    );

    let again = w.bob.pull(&w.bob_client);
    assert_eq!(again.fetched, 0);
    assert_eq!(again.unlinked, 0, "only replies filed this run count");
}
