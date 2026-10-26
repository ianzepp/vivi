use super::*;
use crate::storage::is_traveling_id;

#[test]
fn traveling_ids_match_vivi_shape() {
    let id = mint_traveling_id().unwrap();
    assert!(is_traveling_id(&id), "{id}");
    assert_eq!(handle_of(&id).unwrap().len(), 8);
}

#[test]
fn fold_prefers_the_unreferenced_head() {
    let item = ProjectRecord {
        sequence: 1,
        message_id: "msg_aaaaaaaa0000000000000001".into(),
        from: "alice".into(),
        from_prefix: Some("aaaaaaaa".into()),
        to: Some("hand@bbbbbbbb.local".into()),
        kind: Some("need".into()),
        content_sha256: "x".into(),
        appended_at: "2026-10-12T00:00:00Z".into(),
        raw_eml: b"From: alice@aaaaaaaa.local\nTo: hand@bbbbbbbb.local\n\
             Message-ID: <need-1@aaaaaaaa.local>\nSubject: s\n\nbody\n"
            .to_vec(),
    };
    let done = ProjectRecord {
        sequence: 2,
        message_id: "msg_aaaaaaaa0000000000000002".into(),
        from: "alice".into(),
        from_prefix: Some("aaaaaaaa".into()),
        to: Some("hand@bbbbbbbb.local".into()),
        kind: None,
        content_sha256: "y".into(),
        appended_at: "2026-10-12T00:00:01Z".into(),
        raw_eml: b"From: alice@aaaaaaaa.local\nTo: hand@bbbbbbbb.local\n\
             Message-ID: <done-1@aaaaaaaa.local>\nIn-Reply-To: <need-1@aaaaaaaa.local>\n\
             X-Vivi-Event: done\nX-Vivi-Event-Of: <need-1@aaaaaaaa.local>\n\ndone\n"
            .to_vec(),
    };
    let states = fold_item_states(&[item, done]);
    assert_eq!(states["need-1@aaaaaaaa.local"].state, "done");
}

const AT: &str = "2026-10-12T12:00:00Z";

fn record(
    sequence: u64,
    rfc_id: &str,
    sender: &str,
    kind: Option<&str>,
    appended_at: &str,
    headers: &str,
) -> ProjectRecord {
    let prefix = sender.split_once('@').unwrap().1.split_once('.').unwrap().0;
    ProjectRecord {
        sequence,
        message_id: format!("msg_{sequence:024x}"),
        from: sender.split_once('@').unwrap().0.to_owned(),
        from_prefix: Some(prefix.to_owned()),
        to: Some("hand@bbbbbbbb.local".into()),
        kind: kind.map(str::to_owned),
        content_sha256: format!("{sequence:064x}"),
        appended_at: appended_at.to_owned(),
        raw_eml: format!(
            "From: {sender}\nTo: hand@bbbbbbbb.local\nMessage-ID: <{rfc_id}>\n\
             Subject: s\n{headers}\nbody\n"
        )
        .into_bytes(),
    }
}

fn need(sequence: u64, rfc_id: &str) -> ProjectRecord {
    record(
        sequence,
        rfc_id,
        "alice@aaaaaaaa.local",
        Some("need"),
        AT,
        "",
    )
}

/// An event letter on `of`, written by `sender`, travelling as `mail`
/// like the client's.
fn event(
    sequence: u64,
    rfc_id: &str,
    of: &str,
    sender: &str,
    kind: &str,
    appended_at: &str,
    extra: &str,
) -> ProjectRecord {
    record(
        sequence,
        rfc_id,
        sender,
        None,
        appended_at,
        &format!("In-Reply-To: <{of}>\nX-Vivi-Event: {kind}\nX-Vivi-Event-Of: <{of}>\n{extra}\n"),
    )
}

const BOB: &str = "bob@bbbbbbbb.local";

fn state_of_item(records: &[ProjectRecord], item: &str) -> Option<String> {
    fold_item_states(records)
        .get(item)
        .map(|state| state.state.clone())
}

#[test]
fn a_members_event_closes_another_members_item() {
    let records = [
        need(1, "need-1@aaaaaaaa.local"),
        event(
            2,
            "ev-1@bbbbbbbb.local",
            "need-1@aaaaaaaa.local",
            BOB,
            "done",
            AT,
            "",
        ),
    ];
    assert_eq!(
        state_of_item(&records, "need-1@aaaaaaaa.local").as_deref(),
        Some("done")
    );
}

#[test]
fn an_event_from_a_domain_the_sender_does_not_hold_does_not_count() {
    let item = need(1, "need-1@aaaaaaaa.local");
    // bob is stamped bbbbbbbb but writes as aaaaaaaa: a forged member.
    let mut forged = event(
        2,
        "ev-1@aaaaaaaa.local",
        "need-1@aaaaaaaa.local",
        "alice@aaaaaaaa.local",
        "done",
        AT,
        "",
    );
    forged.from_prefix = Some("bbbbbbbb".into());
    // A legacy name domain is a claim anyone can make.
    let mut legacy = event(
        3,
        "ev-2@bob.local",
        "need-1@aaaaaaaa.local",
        "bob@bbbbbbbb.local",
        "done",
        AT,
        "",
    );
    legacy.raw_eml = String::from_utf8(legacy.raw_eml)
        .unwrap()
        .replace("bob@bbbbbbbb.local", "bob@bob.local")
        .into_bytes();
    // No prefix stamped at all (a legacy identity).
    let mut unstamped = event(
        4,
        "ev-3@bbbbbbbb.local",
        "need-1@aaaaaaaa.local",
        BOB,
        "done",
        AT,
        "",
    );
    unstamped.from_prefix = None;
    let records = [item, forged, legacy, unstamped];
    assert!(!fold_item_states(&records).contains_key("need-1@aaaaaaaa.local"));
}

#[test]
fn an_event_must_reply_to_the_item_it_names() {
    let mut stray = event(
        2,
        "ev-1@bbbbbbbb.local",
        "need-1@aaaaaaaa.local",
        BOB,
        "done",
        AT,
        "",
    );
    stray.raw_eml = String::from_utf8(stray.raw_eml)
        .unwrap()
        .replace("In-Reply-To: <need-1@aaaaaaaa.local>\n", "")
        .into_bytes();
    assert!(
        !fold_item_states(&[need(1, "need-1@aaaaaaaa.local"), stray])
            .contains_key("need-1@aaaaaaaa.local")
    );
}

#[test]
fn the_newer_event_at_wins_whatever_the_append_order() {
    // The reopen was appended first but stamped later: it wins.
    let records = [
        need(1, "need-1@aaaaaaaa.local"),
        event(
            2,
            "ev-1@bbbbbbbb.local",
            "need-1@aaaaaaaa.local",
            BOB,
            "reopened",
            "2026-10-12T12:00:10Z",
            "X-Vivi-Event-At: 2026-10-12T12:00:09.500000Z\n",
        ),
        event(
            3,
            "ev-2@aaaaaaaa.local",
            "need-1@aaaaaaaa.local",
            "alice@aaaaaaaa.local",
            "done",
            "2026-10-12T12:00:20Z",
            "X-Vivi-Event-At: 2026-10-12T12:00:05.000000Z\n",
        ),
    ];
    let states = fold_item_states(&records);
    let state = &states["need-1@aaaaaaaa.local"];
    assert_eq!(state.state, "open");
    assert_eq!(state.event.as_deref(), Some("ev-1@bbbbbbbb.local"));
}

#[test]
fn the_date_header_is_the_instant_when_event_at_is_absent() {
    let records = [
        need(1, "need-1@aaaaaaaa.local"),
        event(
            2,
            "ev-1@bbbbbbbb.local",
            "need-1@aaaaaaaa.local",
            BOB,
            "done",
            "2026-10-12T12:00:00Z",
            "Date: Mon, 12 Oct 2026 12:00:30 +0000\n",
        ),
        event(
            3,
            "ev-2@aaaaaaaa.local",
            "need-1@aaaaaaaa.local",
            "alice@aaaaaaaa.local",
            "reopened",
            "2026-10-12T12:00:40Z",
            "Date: Mon, 12 Oct 2026 12:00:20 +0000\n",
        ),
    ];
    assert_eq!(
        state_of_item(&records, "need-1@aaaaaaaa.local").as_deref(),
        Some("done")
    );
}

#[test]
fn equal_instants_go_to_the_successor_then_the_larger_message_id() {
    let stamp = "X-Vivi-Event-At: 2026-10-12T12:00:05.000000Z\n";
    // zzz-done is the larger id, but reopen names it as Prev: reopen wins.
    let successor = [
        need(1, "need-1@aaaaaaaa.local"),
        event(
            2,
            "zzz-done@bbbbbbbb.local",
            "need-1@aaaaaaaa.local",
            BOB,
            "done",
            AT,
            stamp,
        ),
        event(
            3,
            "aaa-reopen@aaaaaaaa.local",
            "need-1@aaaaaaaa.local",
            "alice@aaaaaaaa.local",
            "reopened",
            AT,
            &format!("{stamp}X-Vivi-Event-Prev: <zzz-done@bbbbbbbb.local>\n"),
        ),
    ];
    assert_eq!(
        state_of_item(&successor, "need-1@aaaaaaaa.local").as_deref(),
        Some("open")
    );
    // No Prev either way: the larger Message-ID wins.
    let by_id = [
        need(1, "need-1@aaaaaaaa.local"),
        event(
            2,
            "zzz-done@bbbbbbbb.local",
            "need-1@aaaaaaaa.local",
            BOB,
            "done",
            AT,
            stamp,
        ),
        event(
            3,
            "aaa-reopen@aaaaaaaa.local",
            "need-1@aaaaaaaa.local",
            "alice@aaaaaaaa.local",
            "reopened",
            AT,
            stamp,
        ),
    ];
    assert_eq!(
        state_of_item(&by_id, "need-1@aaaaaaaa.local").as_deref(),
        Some("done")
    );
}

/// The fold clock the tests below use: just after the fixture's `AT`.
const NOW: &str = "2026-10-12T12:10:00Z";

fn micros(stamp: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(stamp)
        .unwrap()
        .timestamp_micros()
}

fn fold_at(records: &[ProjectRecord], now: &str) -> HashMap<String, ItemState> {
    fold_item_states_at(records, micros(now))
}

/// A close by bob stamped `at`, appended long ago, next to a reopen by
/// alice stamped at the clock.
fn close_then_reopen(at: &str) -> [ProjectRecord; 3] {
    [
        need(1, "need-1@aaaaaaaa.local"),
        event(
            2,
            "ev-1@bbbbbbbb.local",
            "need-1@aaaaaaaa.local",
            BOB,
            "done",
            "2026-10-12T12:00:00Z",
            &format!("X-Vivi-Event-At: {at}\n"),
        ),
        event(
            3,
            "ev-2@aaaaaaaa.local",
            "need-1@aaaaaaaa.local",
            "alice@aaaaaaaa.local",
            "reopened",
            "2026-10-12T12:00:30Z",
            "X-Vivi-Event-At: 2026-10-12T12:00:00.000000Z\n",
        ),
    ]
}

#[test]
fn an_event_eleven_minutes_ahead_is_not_counted_and_nine_is() {
    // The clock is 12:10:00: 12:19 is nine minutes ahead, 12:21 is eleven.
    let nine = close_then_reopen("2026-10-12T12:19:00.000000Z");
    let states = fold_at(&nine, NOW);
    let state = &states["need-1@aaaaaaaa.local"];
    assert_eq!(state.state, "done", "nine minutes ahead counts");
    assert_eq!(state.event.as_deref(), Some("ev-1@bbbbbbbb.local"));
    assert_eq!(state.newest_at, micros("2026-10-12T12:19:00Z"));

    let eleven = close_then_reopen("2026-10-12T12:21:00.000000Z");
    let states = fold_at(&eleven, NOW);
    let state = &states["need-1@aaaaaaaa.local"];
    assert_eq!(state.state, "open", "eleven minutes ahead is not counted");
    assert_eq!(state.event.as_deref(), Some("ev-2@aaaaaaaa.local"));
    // Not counted means not there: the instant is not a floor either.
    assert_eq!(state.newest_at, micros("2026-10-12T12:00:00Z"));
    assert_eq!(state.heads, ["ev-2@aaaaaaaa.local"]);

    // Alone, an event that is not counted leaves no state at all.
    let states = fold_at(&eleven[..2], NOW);
    assert!(!states.contains_key("need-1@aaaaaaaa.local"));
}

#[test]
fn the_future_bound_is_exactly_ten_minutes_of_the_clock() {
    let at_limit = close_then_reopen("2026-10-12T12:20:00.000000Z");
    assert_eq!(
        fold_at(&at_limit, NOW)["need-1@aaaaaaaa.local"].state,
        "done",
        "exactly ten minutes ahead still counts"
    );
    let past = close_then_reopen("2026-10-12T12:20:00.000001Z");
    assert_eq!(
        fold_at(&past, NOW)["need-1@aaaaaaaa.local"].state,
        "open",
        "one microsecond more does not"
    );
}

#[test]
fn the_bound_is_read_at_fold_time_not_at_append_time() {
    // Appended at 12:00:00 and stamped 12:21: eleven minutes ahead of a
    // 12:10 clock, but only nine of a 12:12 clock. The same letters fold
    // differently as the clock moves, as on the client.
    let records = close_then_reopen("2026-10-12T12:21:00.000000Z");
    assert_eq!(
        fold_at(&records, "2026-10-12T12:10:00Z")["need-1@aaaaaaaa.local"].state,
        "open"
    );
    assert_eq!(
        fold_at(&records, "2026-10-12T12:12:00Z")["need-1@aaaaaaaa.local"].state,
        "done"
    );
    // And a far-future stamp is not the append time: it counts for nothing
    // at any clock short of its own.
    let forever = close_then_reopen("2099-01-01T00:00:00.000000Z");
    assert_eq!(
        fold_at(&forever, "2026-10-12T12:59:00Z")["need-1@aaaaaaaa.local"].state,
        "open"
    );
    assert!(!fold_at(&forever[..2], "2026-10-12T12:59:00Z").contains_key("need-1@aaaaaaaa.local"));
}

#[test]
fn a_future_property_event_is_not_counted_either() {
    let records = [
        need(1, "need-1@aaaaaaaa.local"),
        event(
            2,
            "m1@bbbbbbbb.local",
            "need-1@aaaaaaaa.local",
            BOB,
            "marked",
            AT,
            "X-Vivi-Event-Slug: soon\nX-Vivi-Event-At: 2026-10-12T12:21:00.000000Z\n",
        ),
        event(
            3,
            "m2@bbbbbbbb.local",
            "need-1@aaaaaaaa.local",
            BOB,
            "marked",
            AT,
            "X-Vivi-Event-Slug: now\nX-Vivi-Event-At: 2026-10-12T12:19:00.000000Z\n",
        ),
    ];
    let states = fold_at(&records, NOW);
    assert_eq!(states["need-1@aaaaaaaa.local"].marks, ["now"]);
}

/// A state event on need-1 with the instant every event below shares.
fn tied(sequence: u64, id: &str, sender: &str, kind: &str, prev: &[&str]) -> ProjectRecord {
    let prevs: String = prev
        .iter()
        .map(|id| format!("X-Vivi-Event-Prev: <{id}>\n"))
        .collect();
    event(
        sequence,
        id,
        "need-1@aaaaaaaa.local",
        sender,
        kind,
        AT,
        &format!("X-Vivi-Event-At: 2026-10-12T12:00:05.000000Z\n{prevs}"),
    )
}

#[test]
fn causal_depth_orders_a_three_event_chain_with_equal_instants() {
    // done -> reopened -> done, every instant equal. The last names the
    // middle as Prev and the middle names the first, so depths are 0, 1,
    // 2 and the last wins. Its Message-ID is the smallest of the three:
    // neither a direct-successor test (the last does not name the first)
    // nor the id alone can pick it.
    let held = |index: usize| match index {
        0 => tied(2, "zzz-first@bbbbbbbb.local", BOB, "done", &[]),
        1 => tied(
            3,
            "mmm-middle@aaaaaaaa.local",
            "alice@aaaaaaaa.local",
            "reopened",
            &["zzz-first@bbbbbbbb.local"],
        ),
        _ => tied(
            4,
            "aaa-last@bbbbbbbb.local",
            BOB,
            "done",
            &["mmm-middle@aaaaaaaa.local"],
        ),
    };
    let orders: [[usize; 3]; 6] = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];
    for order in orders {
        let mut records = vec![need(1, "need-1@aaaaaaaa.local")];
        records.extend(order.iter().map(|&index| held(index)));
        let states = fold_at(&records, NOW);
        let state = &states["need-1@aaaaaaaa.local"];
        assert_eq!(state.state, "done", "order {order:?}");
        assert_eq!(
            state.event.as_deref(),
            Some("aaa-last@bbbbbbbb.local"),
            "order {order:?}"
        );
        assert_eq!(state.heads, ["aaa-last@bbbbbbbb.local"], "order {order:?}");
    }

    // Flip the end of the chain: the deeper event is the reopen now.
    let flipped = tied(
        4,
        "aaa-last@bbbbbbbb.local",
        BOB,
        "reopened",
        &["mmm-middle@aaaaaaaa.local"],
    );
    let records = [need(1, "need-1@aaaaaaaa.local"), held(0), held(1), flipped];
    assert_eq!(
        state_of_item(&records, "need-1@aaaaaaaa.local").as_deref(),
        Some("open")
    );
}

#[test]
fn a_fork_with_equal_instants_and_depths_goes_to_the_larger_message_id() {
    let root = tied(2, "root@bbbbbbbb.local", BOB, "done", &[]);
    let left = tied(
        3,
        "left@aaaaaaaa.local",
        "alice@aaaaaaaa.local",
        "reopened",
        &["root@bbbbbbbb.local"],
    );
    let right = tied(
        4,
        "right@bbbbbbbb.local",
        BOB,
        "done",
        &["root@bbbbbbbb.local"],
    );
    let states = fold_at(&[need(1, "need-1@aaaaaaaa.local"), root, left, right], NOW);
    let state = &states["need-1@aaaaaaaa.local"];
    // Both children sit at depth 1: the larger id, `right`, decides.
    assert_eq!(state.state, "done");
    assert_eq!(state.event.as_deref(), Some("right@bbbbbbbb.local"));
    // A new event follows both heads, oldest first by the order key.
    assert_eq!(state.heads, ["left@aaaaaaaa.local", "right@bbbbbbbb.local"]);
}

#[test]
fn a_later_instant_beats_a_deeper_chain() {
    // The reopen names the close as Prev (depth 1) but is stamped before
    // an unrelated close: the clock beats the chain.
    let first = tied(2, "first@bbbbbbbb.local", BOB, "done", &[]);
    let reopen = tied(
        3,
        "reopen@aaaaaaaa.local",
        "alice@aaaaaaaa.local",
        "reopened",
        &["first@bbbbbbbb.local"],
    );
    let later = event(
        4,
        "later@bbbbbbbb.local",
        "need-1@aaaaaaaa.local",
        BOB,
        "done",
        AT,
        "X-Vivi-Event-At: 2026-10-12T12:00:06.000000Z\n",
    );
    let states = fold_at(
        &[need(1, "need-1@aaaaaaaa.local"), first, reopen, later],
        NOW,
    );
    assert_eq!(states["need-1@aaaaaaaa.local"].state, "done");
    assert_eq!(
        states["need-1@aaaaaaaa.local"].event.as_deref(),
        Some("later@bbbbbbbb.local")
    );
}

#[test]
fn a_prev_that_names_nothing_counted_adds_no_depth() {
    // `early` names a future-dated event and one never held: depth 0, so
    // an unrelated depth-0 event with the larger id beats it.
    let future = event(
        2,
        "future@bbbbbbbb.local",
        "need-1@aaaaaaaa.local",
        BOB,
        "done",
        AT,
        "X-Vivi-Event-At: 2026-10-12T13:00:00.000000Z\n",
    );
    let early = tied(
        3,
        "aaa-early@aaaaaaaa.local",
        "alice@aaaaaaaa.local",
        "reopened",
        &["future@bbbbbbbb.local", "never-held@bbbbbbbb.local"],
    );
    let rival = tied(4, "zzz-rival@bbbbbbbb.local", BOB, "done", &[]);
    let states = fold_at(
        &[need(1, "need-1@aaaaaaaa.local"), future, early, rival],
        NOW,
    );
    let state = &states["need-1@aaaaaaaa.local"];
    assert_eq!(state.event.as_deref(), Some("zzz-rival@bbbbbbbb.local"));
    assert_eq!(state.state, "done");
}

#[test]
fn a_cycle_of_prevs_folds_and_a_long_chain_does_not_overflow() {
    // Two events naming each other: no honest writer makes one, but the
    // fold must still end, and name one of them.
    let a = tied(2, "a@bbbbbbbb.local", BOB, "done", &["b@aaaaaaaa.local"]);
    let b = tied(
        3,
        "b@aaaaaaaa.local",
        "alice@aaaaaaaa.local",
        "reopened",
        &["a@bbbbbbbb.local"],
    );
    let states = fold_at(&[need(1, "need-1@aaaaaaaa.local"), a, b], NOW);
    assert!(states.contains_key("need-1@aaaaaaaa.local"));

    // A member can append as long a chain as it likes; depth is walked
    // without recursion, so this does not run the stack out.
    let mut records = vec![need(1, "need-1@aaaaaaaa.local")];
    let length = 50_000u64;
    for n in 0..length {
        let prev = format!("link-{}@bbbbbbbb.local", n.wrapping_sub(1));
        let prevs: Vec<&str> = if n == 0 { Vec::new() } else { vec![&prev] };
        let kind = if n % 2 == 0 { "done" } else { "reopened" };
        records.push(tied(
            2 + n,
            &format!("link-{n}@bbbbbbbb.local"),
            BOB,
            kind,
            &prevs,
        ));
    }
    let states = fold_at(&records, NOW);
    let state = &states["need-1@aaaaaaaa.local"];
    assert_eq!(
        state.event.as_deref(),
        Some(format!("link-{}@bbbbbbbb.local", length - 1).as_str())
    );
    assert_eq!(state.heads.len(), 1);
}

#[test]
fn absorbed_is_final_for_later_events() {
    let sealed_done = [
        need(1, "need-1@aaaaaaaa.local"),
        event(
            2,
            "seal@bbbbbbbb.local",
            "need-1@aaaaaaaa.local",
            BOB,
            "absorbed",
            AT,
            "X-Vivi-Event-At: 2026-10-12T12:00:05.000000Z\nX-Vivi-Event-State: done\n",
        ),
        event(
            3,
            "late@aaaaaaaa.local",
            "need-1@aaaaaaaa.local",
            "alice@aaaaaaaa.local",
            "reopened",
            "2026-10-12T12:01:00Z",
            "X-Vivi-Event-At: 2026-10-12T12:01:00.000000Z\n",
        ),
    ];
    let states = fold_item_states(&sealed_done);
    let state = &states["need-1@aaaaaaaa.local"];
    assert_eq!(state.state, "done");
    assert!(state.absorbed);
    assert_eq!(state.event.as_deref(), Some("seal@bbbbbbbb.local"));

    // Sealed open: a done, even a newer one, changes nothing.
    let sealed_open = [
        need(1, "need-1@aaaaaaaa.local"),
        event(
            2,
            "seal@bbbbbbbb.local",
            "need-1@aaaaaaaa.local",
            BOB,
            "absorbed",
            AT,
            "X-Vivi-Event-At: 2026-10-12T12:00:05.000000Z\nX-Vivi-Event-State: open\n",
        ),
        event(
            3,
            "late@aaaaaaaa.local",
            "need-1@aaaaaaaa.local",
            "alice@aaaaaaaa.local",
            "done",
            "2026-10-12T12:01:00Z",
            "X-Vivi-Event-At: 2026-10-12T12:01:00.000000Z\n",
        ),
    ];
    assert_eq!(
        state_of_item(&sealed_open, "need-1@aaaaaaaa.local").as_deref(),
        Some("open")
    );
}

#[test]
fn the_earliest_seal_wins_between_two_absorbs() {
    let seal = |sequence: u64, id: &str, sender: &str, state: &str, at: &str| {
        event(
            sequence,
            id,
            "need-1@aaaaaaaa.local",
            sender,
            "absorbed",
            AT,
            &format!("X-Vivi-Event-At: {at}\nX-Vivi-Event-State: {state}\n"),
        )
    };
    let records = [
        need(1, "need-1@aaaaaaaa.local"),
        seal(
            2,
            "late@aaaaaaaa.local",
            "alice@aaaaaaaa.local",
            "open",
            "2026-10-12T12:00:09.000000Z",
        ),
        seal(
            3,
            "early@bbbbbbbb.local",
            BOB,
            "done",
            "2026-10-12T12:00:03.000000Z",
        ),
    ];
    assert_eq!(
        state_of_item(&records, "need-1@aaaaaaaa.local").as_deref(),
        Some("done")
    );
}

#[test]
fn marks_fold_the_newest_letter_per_slug() {
    let mark = |sequence: u64, id: &str, kind: &str, slug: &str, at: &str| {
        event(
            sequence,
            id,
            "need-1@aaaaaaaa.local",
            BOB,
            kind,
            AT,
            &format!("X-Vivi-Event-Slug: {slug}\nX-Vivi-Event-At: {at}\n"),
        )
    };
    let records = [
        need(1, "need-1@aaaaaaaa.local"),
        mark(
            2,
            "m1@bbbbbbbb.local",
            "marked",
            "bug",
            "2026-10-12T12:00:01.000000Z",
        ),
        mark(
            3,
            "m2@bbbbbbbb.local",
            "unmarked",
            "bug",
            "2026-10-12T12:00:02.000000Z",
        ),
        mark(
            4,
            "m3@bbbbbbbb.local",
            "marked",
            "bug",
            "2026-10-12T12:00:03.000000Z",
        ),
        mark(
            5,
            "m4@bbbbbbbb.local",
            "marked",
            "regression",
            "2026-10-12T12:00:04.000000Z",
        ),
        // Appended last, stamped before the newest letter for the slug.
        mark(
            6,
            "m5@bbbbbbbb.local",
            "unmarked",
            "regression",
            "2026-10-12T12:00:03.500000Z",
        ),
        mark(
            7,
            "m6@bbbbbbbb.local",
            "marked",
            "gone",
            "2026-10-12T12:00:01.000000Z",
        ),
        mark(
            8,
            "m7@bbbbbbbb.local",
            "unmarked",
            "gone",
            "2026-10-12T12:00:02.000000Z",
        ),
    ];
    let states = fold_item_states(&records);
    assert_eq!(states["need-1@aaaaaaaa.local"].marks, ["bug", "regression"]);
    // Marks do not touch state.
    assert_eq!(states["need-1@aaaaaaaa.local"].state, "open");
}

#[test]
fn a_sealed_item_takes_no_later_marks() {
    let records = [
        need(1, "need-1@aaaaaaaa.local"),
        event(
            2,
            "m1@bbbbbbbb.local",
            "need-1@aaaaaaaa.local",
            BOB,
            "marked",
            AT,
            "X-Vivi-Event-Slug: bug\nX-Vivi-Event-At: 2026-10-12T12:00:01.000000Z\n",
        ),
        event(
            3,
            "seal@bbbbbbbb.local",
            "need-1@aaaaaaaa.local",
            BOB,
            "absorbed",
            AT,
            "X-Vivi-Event-State: open\nX-Vivi-Event-At: 2026-10-12T12:00:02.000000Z\n",
        ),
        event(
            4,
            "m2@bbbbbbbb.local",
            "need-1@aaaaaaaa.local",
            BOB,
            "marked",
            AT,
            "X-Vivi-Event-Slug: late\nX-Vivi-Event-At: 2026-10-12T12:00:03.000000Z\n",
        ),
    ];
    let states = fold_item_states(&records);
    assert_eq!(states["need-1@aaaaaaaa.local"].marks, ["bug"]);
}

#[test]
fn the_newest_bind_controls_a_unit() {
    let bind = |sequence: u64, id: &str, of: &str, units: &[&str], at: &str| {
        let units: String = units
            .iter()
            .map(|unit| format!("X-Vivi-Event-Unit: <{unit}>\n"))
            .collect();
        event(
            sequence,
            id,
            of,
            BOB,
            "bound",
            AT,
            &format!("X-Vivi-Event-At: {at}\n{units}"),
        )
    };
    let records = [
        need(1, "need-1@aaaaaaaa.local"),
        need(2, "need-2@aaaaaaaa.local"),
        bind(
            3,
            "b1@bbbbbbbb.local",
            "need-1@aaaaaaaa.local",
            &["unit-a@aaaaaaaa.local", "unit-b@aaaaaaaa.local"],
            "2026-10-12T12:00:05.000000Z",
        ),
        // Appended after, stamped before: moves nothing.
        bind(
            4,
            "b2@bbbbbbbb.local",
            "need-2@aaaaaaaa.local",
            &["unit-a@aaaaaaaa.local"],
            "2026-10-12T12:00:01.000000Z",
        ),
        // Newer: unit-b moves to need-2.
        bind(
            5,
            "b3@bbbbbbbb.local",
            "need-2@aaaaaaaa.local",
            &["unit-b@aaaaaaaa.local"],
            "2026-10-12T12:00:09.000000Z",
        ),
    ];
    let states = fold_item_states(&records);
    assert_eq!(
        states["need-1@aaaaaaaa.local"].units,
        ["unit-a@aaaaaaaa.local"]
    );
    assert_eq!(
        states["need-2@aaaaaaaa.local"].units,
        ["unit-b@aaaaaaaa.local"]
    );
}

#[test]
fn unknown_event_kinds_and_unpayloaded_events_are_skipped() {
    let records = [
        need(1, "need-1@aaaaaaaa.local"),
        event(
            2,
            "ev-1@bbbbbbbb.local",
            "need-1@aaaaaaaa.local",
            BOB,
            "snoozed",
            AT,
            "",
        ),
        // marked with no slug, bound with no unit, absorbed with no state
        event(
            3,
            "ev-2@bbbbbbbb.local",
            "need-1@aaaaaaaa.local",
            BOB,
            "marked",
            AT,
            "",
        ),
        event(
            4,
            "ev-3@bbbbbbbb.local",
            "need-1@aaaaaaaa.local",
            BOB,
            "bound",
            AT,
            "",
        ),
        event(
            5,
            "ev-4@bbbbbbbb.local",
            "need-1@aaaaaaaa.local",
            BOB,
            "absorbed",
            AT,
            "",
        ),
    ];
    assert!(fold_item_states(&records).is_empty());
}

#[test]
fn an_event_letter_is_never_an_item_status() {
    let item = need(1, "need-1@aaaaaaaa.local");
    let done = event(
        2,
        "ev-1@bbbbbbbb.local",
        "need-1@aaaaaaaa.local",
        BOB,
        "done",
        AT,
        "",
    );
    let states = fold_item_states(&[
        need(1, "need-1@aaaaaaaa.local"),
        event(
            2,
            "ev-1@bbbbbbbb.local",
            "need-1@aaaaaaaa.local",
            BOB,
            "done",
            AT,
            "",
        ),
    ]);
    assert_eq!(
        item_status(&item, &parse_letter(&item.raw_eml), &states),
        "done"
    );
    // The event is no item under any envelope kind: `mail`, as the
    // client sends it, or the item's own kind, as an older one did.
    assert_eq!(done.kind, None);
    assert_eq!(
        item_status(&done, &parse_letter(&done.raw_eml), &states),
        "event"
    );
    let mut riding = event(
        3,
        "ev-2@bbbbbbbb.local",
        "need-1@aaaaaaaa.local",
        BOB,
        "reopened",
        AT,
        "",
    );
    riding.kind = Some("need".into());
    assert_eq!(
        item_status(&riding, &parse_letter(&riding.raw_eml), &states),
        "event"
    );
}
