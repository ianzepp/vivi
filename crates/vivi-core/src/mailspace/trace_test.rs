use std::collections::{BTreeSet, HashMap};

use super::{
    HandleIndexes, TraceContent, TraceIndex, citing_contents, handle_window, strip_reply_prefix,
    thread_candidate,
};

fn content(content_id: &str, handle: &str, date: &str, subject: &str) -> TraceContent {
    TraceContent {
        content_id: content_id.into(),
        message_id: format!("msg_{content_id}"),
        handle: handle.into(),
        date: date.into(),
        subject: subject.into(),
        participants: BTreeSet::from(["a@example.com".to_string()]),
        blob_relpath: String::new(),
        copies: Vec::new(),
    }
}

#[test]
fn handle_window_accepts_only_eight_lowercase_hex_characters() {
    assert!(handle_window("014a275f").is_some());
    // Seven characters, seven digits and a letter, and uppercase hex are not
    // windows: the body scan keys on exactly the handle width.
    assert!(handle_window("014a275").is_none());
    assert!(handle_window("014a275fg").is_none());
    assert!(handle_window("014A275F").is_none());
}

#[test]
fn citation_scan_finds_handles_anywhere_in_a_body() {
    let indexes = HandleIndexes::build(&[
        content("c-one", "014a275f", "2026-01-01T00:00:00Z", "one"),
        content("c-two", "023b3da8", "2026-01-02T00:00:00Z", "two"),
    ]);

    // Bare citation, and a handle embedded inside a longer token.
    assert_eq!(
        citing_contents(
            "see 014a275f for detail",
            &indexes.windows,
            &indexes.odd_handles
        ),
        vec![0]
    );
    assert_eq!(
        citing_contents(
            "id msg_023b3da8aaaabbbb",
            &indexes.windows,
            &indexes.odd_handles
        ),
        vec![1]
    );
    assert!(citing_contents("nothing here", &indexes.windows, &indexes.odd_handles).is_empty());
    // A seven-character token is not the eight-character handle.
    assert!(citing_contents("014a275", &indexes.windows, &indexes.odd_handles).is_empty());
}

#[test]
fn citation_scan_supports_handles_that_are_not_eight_hex_characters() {
    let indexes = HandleIndexes::build(&[
        content("c-one", "not-a-handle", "2026-01-01T00:00:00Z", "one"),
        content("c-two", "014a275f", "2026-01-02T00:00:00Z", "two"),
    ]);

    assert_eq!(
        citing_contents(
            "quoted not-a-handle inline",
            &indexes.windows,
            &indexes.odd_handles
        ),
        vec![0]
    );
    assert_eq!(
        citing_contents("plain 014a275f", &indexes.windows, &indexes.odd_handles),
        vec![1]
    );
}

#[test]
fn thread_candidate_takes_the_newest_earlier_content() {
    let contents = vec![
        content(
            "c-old",
            "aaaaaaaa",
            "2026-01-01T00:00:00Z",
            "status: blocker",
        ),
        content(
            "c-mid",
            "bbbbbbbb",
            "2026-01-02T00:00:00Z",
            "Re: status: blocker",
        ),
        content(
            "c-new",
            "cccccccc",
            "2026-01-03T00:00:00Z",
            "Re: Re: status: blocker",
        ),
    ];
    let bucket: Vec<usize> = vec![0, 1, 2];

    // The newest content has no newer sibling, so it takes the middle one.
    assert_eq!(thread_candidate(&contents, &bucket, 2), Some(1));
    // The oldest content is never its own parent.
    assert_eq!(thread_candidate(&contents, &bucket, 0), None);
}

#[test]
fn thread_candidate_ignores_newer_contents() {
    let contents = vec![
        content(
            "c-new",
            "cccccccc",
            "2026-01-03T00:00:00Z",
            "status: blocker",
        ),
        content(
            "c-old",
            "aaaaaaaa",
            "2026-01-01T00:00:00Z",
            "status: blocker",
        ),
    ];

    // The bucket is ordered oldest first; the child only looks backwards.
    assert_eq!(thread_candidate(&contents, &[1, 0], 1), None);
}

#[test]
fn reply_prefixes_strip_repeatedly_and_lowercase() {
    assert_eq!(strip_reply_prefix("Re: Status: Blocker"), "status: blocker");
    assert_eq!(
        strip_reply_prefix("RE: Re: status: blocker"),
        "status: blocker"
    );
    // A word that merely starts with "re" is part of the subject.
    assert_eq!(strip_reply_prefix("Release notes"), "release notes");
}

/// An index over hand-built contents: enough to exercise the inferred-parent
/// lookups without a mailspace.
fn test_index(contents: Vec<TraceContent>) -> TraceIndex {
    let indexes = HandleIndexes::build(&contents);
    TraceIndex {
        by_content: contents
            .iter()
            .enumerate()
            .map(|(position, content)| (content.content_id.clone(), position))
            .collect(),
        by_handle: indexes.by_handle,
        content_of_message: HashMap::new(),
        windows: indexes.windows,
        odd_handles: indexes.odd_handles,
        threads: super::build_threads(&contents),
        links: HashMap::new(),
        events: super::EventIndex {
            events: Vec::new(),
            by_message: HashMap::new(),
        },
        contents,
    }
}

#[test]
fn content_for_token_resolves_handles_and_full_ids_only() {
    let mut index = test_index(vec![
        content("c-one", "014a275f", "2026-01-01T00:00:00Z", "one"),
        content("c-two", "014a275f", "2026-01-02T00:00:00Z", "two"),
    ]);
    index
        .content_of_message
        .insert("msg_idonly".into(), "c-one".into());

    // A handle two contents share is ambiguous, matching token resolution.
    assert_eq!(index.content_for_token("014a275f"), None);
    assert_eq!(
        index.content_for_token("msg_idonly"),
        Some("c-one".to_string())
    );
    assert_eq!(index.content_for_token("ffffffff"), None);
}

#[test]
fn citation_ties_resolve_by_content_id_not_body_order() {
    // Two candidates share a date; the body cites both.
    let index = test_index(vec![
        content("c-aaa", "aaaaaaaa", "2026-01-01T00:00:00Z", "one"),
        content("c-bbb", "bbbbbbbb", "2026-01-01T00:00:00Z", "two"),
        content("c-child", "cccccccc", "2026-01-02T00:00:00Z", "three"),
    ]);

    // The lowest content id wins the tie, whichever order the body cites them.
    assert_eq!(index.cited_parent(2, "see aaaaaaaa then bbbbbbbb"), Some(0));
    assert_eq!(index.cited_parent(2, "see bbbbbbbb then aaaaaaaa"), Some(0));
    // A candidate newer than the child is never its parent.
    assert_eq!(index.cited_parent(0, "see cccccccc"), None);
}
