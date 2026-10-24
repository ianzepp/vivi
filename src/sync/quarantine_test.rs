//! W3-B5: a pull that meets a letter it cannot read holds it under
//! `.vivi/quarantine/` and names it, instead of refusing silently or
//! dropping it. Two temp mailspaces sync through the fake server; the
//! broken letter reaches A's store through a raw ingest, bypassing the
//! compose gate (the newer-member path: A's vivi accepted what B's cannot
//! read).

use std::rc::Rc;

use super::*;
use crate::letter::fixture::need_body;
use crate::storage::{MessageIngestRequest, sha256_hex};
use crate::sync::fake_server::{Checkout, FakeServer, checkout};
use crate::sync::pull::{self, PullPlan};

fn pair() -> (Rc<FakeServer>, Checkout, Checkout) {
    let server = FakeServer::new();
    let a = checkout("alice", &["mind"]);
    let b = checkout("bob", &["mind"]);
    (server, a, b)
}

/// Raw ingest into a mailspace's store, bypassing the compose gate: the
/// shape a newer member's send leaves behind on its own machine.
fn put(fx: &Checkout, account: &str, role: &str, bytes: &[u8]) -> String {
    fx.mailspace
        .storage()
        .unwrap()
        .ingest_message(
            &MessageIngestRequest {
                account: account.into(),
                local_role: role.into(),
                read_state: false,
                starred: false,
                message_id_hint: None,
                presented_id: None,
                seed_hint: format!("quarantine\0{role}\0{}", bytes.len()),
                remote: None,
            },
            bytes,
        )
        .unwrap()
        .message_id
}

fn letter(from: &str, to: &str, subject: &str, extra_headers: &str, body: &str) -> Vec<u8> {
    format!(
        "From: {from}\r\nTo: {to}\r\nSubject: {subject}\r\nMessage-ID: <{subject}@x.test>\r\n\
         Date: Sun, 11 Oct 2026 12:00:00 +0000\r\n{extra_headers}\r\n{body}"
    )
    .into_bytes()
}

/// A format-1 need whose front matter carries a key this vivi does not
/// know: what a newer member sends.
fn newer_member_need(from: &str, to: &str, subject: &str) -> Vec<u8> {
    letter(
        from,
        to,
        subject,
        "X-Vivi-Kind: need\r\nX-Vivi-Format: 1\r\n",
        "+++\ndone_when = [\"it works\"]\nflavor = \"future\"\n+++\n\nThe work.\n",
    )
}

/// A format-1 need this vivi reads.
fn good_need(from: &str, to: &str, subject: &str) -> Vec<u8> {
    letter(
        from,
        to,
        subject,
        "X-Vivi-Kind: need\r\nX-Vivi-Format: 1\r\n",
        &need_body(&["it works"], "The work.\n"),
    )
}

/// A letter with no `X-Vivi-Format`: pre-format history.
fn legacy_need(from: &str, to: &str, subject: &str) -> Vec<u8> {
    letter(from, to, subject, "X-Vivi-Kind: need\r\n", "Plain prose.\n")
}

fn quarantine_root(b: &Checkout) -> std::path::PathBuf {
    dir(&b.mailspace.dir)
}

fn live_count(b: &Checkout, sha: &str) -> usize {
    b.mailspace
        .storage()
        .unwrap()
        .live_messages_for_content(sha)
        .unwrap()
        .len()
}

#[test]
fn an_unknown_front_key_letter_is_held_named_and_not_filed() {
    let (server, a, b) = pair();
    let bytes = newer_member_need(&a.address("mind"), &b.address("mind"), "future need");
    let wire_id = put(&a, "mind", "inbox", &bytes);
    a.push(&a.client(&server));

    let report = b.pull(&b.client(&server));

    let sha = sha256_hex(&bytes);
    assert_eq!(report.quarantined.len(), 1, "{}", report.summary());
    assert_eq!(report.quarantined[0].id, sha);
    assert!(
        report.quarantined[0].reason.contains("flavor"),
        "{}",
        report.quarantined[0].reason
    );
    assert!(
        report.refused.is_empty(),
        "a letter-parse failure is not a transport refusal"
    );
    assert!(
        report
            .summary()
            .contains(&format!("1 quarantined: {sha} (")),
        "{}",
        report.summary()
    );
    let failure = report
        .failure()
        .expect("a quarantined letter fails the pull");
    assert!(failure.contains(&sha), "{failure}");

    let root = quarantine_root(&b);
    let eml = std::fs::read(root.join(format!("{sha}.eml"))).unwrap();
    assert_eq!(eml, bytes, "the exact bytes are held");
    let raw = std::fs::read_to_string(root.join(format!("{sha}.json"))).unwrap();
    let receipt: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(receipt["message_id"], wire_id);
    assert_eq!(receipt["from"], "alice");
    assert_eq!(receipt["kind"], "need");
    assert_eq!(receipt["sequence"], 1);
    assert_eq!(live_count(&b, &sha), 0, "the letter is held, not filed");

    // A second pull is silent: no new record, no alarm, exit zero.
    let again = b.pull(&b.client(&server));
    assert!(again.quarantined.is_empty(), "{}", again.summary());
    assert!(again.failure().is_none());

    // The same content arriving again (a resync replays the log) finds the
    // held file and does not alarm again, and still files nothing.
    let replayed = pull::pull_with(
        &b.mailspace,
        &b.settings,
        &b.client(&server),
        &PullPlan {
            resync: true,
            ..PullPlan::default()
        },
        &mut |_| {},
    )
    .unwrap();
    assert!(replayed.quarantined.is_empty(), "{}", replayed.summary());
    assert!(replayed.failure().is_none());
    assert_eq!(
        live_count(&b, &sha),
        0,
        "a replay of a held letter files nothing"
    );
}

#[test]
fn a_synced_legacy_letter_still_files_as_history() {
    let (server, a, b) = pair();
    let bytes = legacy_need(&a.address("mind"), &b.address("mind"), "old need");
    put(&a, "mind", "inbox", &bytes);
    a.push(&a.client(&server));

    let report = b.pull(&b.client(&server));

    assert_eq!(report.filed.get("needs"), Some(&1), "{}", report.summary());
    assert!(report.quarantined.is_empty(), "{}", report.summary());
    assert!(report.failure().is_none());
    assert!(
        !quarantine_root(&b).exists(),
        "a legacy letter never reaches quarantine"
    );
}

#[test]
fn a_good_format_one_letter_files_normally() {
    let (server, a, b) = pair();
    let bytes = good_need(&a.address("mind"), &b.address("mind"), "plain need");
    put(&a, "mind", "inbox", &bytes);
    a.push(&a.client(&server));

    let report = b.pull(&b.client(&server));

    assert_eq!(report.filed.get("needs"), Some(&1), "{}", report.summary());
    assert!(report.quarantined.is_empty(), "{}", report.summary());
    assert_eq!(live_count(&b, &sha256_hex(&bytes)), 1);
}

fn receipt(message_id: &str, sequence: u64, reason: &str) -> HeldLetter {
    HeldLetter {
        id: String::new(),
        message_id: message_id.to_string(),
        from: "alice".to_string(),
        kind: Some("need".to_string()),
        sequence,
        reason: reason.to_string(),
        at: "2026-10-24T12:00:00+00:00".to_string(),
    }
}

#[test]
fn the_same_content_holds_once_and_the_receipt_shape_is_fixed() {
    let (.., b) = pair();
    let bytes = newer_member_need("mind@00112233.local", "mind@feedface.local", "future");
    let held = receipt(
        "msg_0000000000000000000000aa",
        4,
        "front matter line 3: unknown field",
    );
    assert!(
        hold(&b.mailspace.dir, &bytes, &held).unwrap(),
        "the first hold alarms"
    );
    let sha = sha256_hex(&bytes);
    let json = quarantine_root(&b).join(format!("{sha}.json"));
    let first = std::fs::read_to_string(&json).unwrap();

    let mut again = receipt("msg_0000000000000000000000ff", 9, "a different reason");
    again.at = "2027-01-01T00:00:00+00:00".to_string();
    assert!(
        !hold(&b.mailspace.dir, &bytes, &again).unwrap(),
        "the second hold is silent"
    );
    assert_eq!(
        std::fs::read_to_string(&json).unwrap(),
        first,
        "a silent hold rewrites nothing"
    );

    let value: serde_json::Value = serde_json::from_str(&first).unwrap();
    let mut keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["at", "from", "kind", "message_id", "reason", "sequence"],
        "{first}"
    );

    let listed = list(&b.mailspace.dir).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, sha);
    assert_eq!(listed[0].message_id, held.message_id);
    assert_eq!(
        read_bytes(&b.mailspace.dir, &sha).unwrap(),
        bytes,
        "the exact bytes read back"
    );
}

#[test]
fn discard_moves_the_pair_to_dropped_and_ids_are_checked() {
    let (.., b) = pair();
    let bytes = newer_member_need("mind@00112233.local", "mind@feedface.local", "future");
    let note = receipt("msg_0000000000000000000000bb", 5, "unknown field");
    hold(&b.mailspace.dir, &bytes, &note).unwrap();
    let sha = sha256_hex(&bytes);

    discard(&b.mailspace.dir, &sha).unwrap();

    assert!(list(&b.mailspace.dir).unwrap().is_empty(), "nothing held");
    let dropped = quarantine_root(&b).join("dropped");
    assert_eq!(
        std::fs::read(dropped.join(format!("{sha}.eml"))).unwrap(),
        bytes
    );
    assert!(dropped.join(format!("{sha}.json")).exists());
    assert!(
        discard(&b.mailspace.dir, &sha).is_err(),
        "nothing left to drop"
    );

    // An id that is not a content sha256 is refused, never treated as a path.
    assert!(read_bytes(&b.mailspace.dir, "../../mailspace.toml").is_err());
    assert!(discard(&b.mailspace.dir, "zz").is_err());
    assert!(held(&b.mailspace.dir, "nope").is_err());
}

#[test]
fn refile_files_a_held_letter_the_parser_accepts_and_drops_the_pair() {
    let (.., b) = pair();
    // A good letter held the way an older vivi would have held it.
    let bytes = good_need("mind@00112233.local", &b.address("mind"), "healed need");
    let id = sha256_hex(&bytes);
    let held = receipt("msg_0000000000000000000000cc", 6, "unknown field `flavor`");
    hold(&b.mailspace.dir, &bytes, &held).unwrap();

    let outcome = refile(&b.mailspace, &b.settings, &id).unwrap();

    assert_eq!(outcome, RefileOutcome::Filed);
    assert_eq!(live_count(&b, &id), 1, "the letter is filed");
    assert_eq!(b.rows().len(), 1);
    assert_eq!(b.rows()[0].local_role, "needs");
    assert!(
        list(&b.mailspace.dir).unwrap().is_empty(),
        "the pair moved out of quarantine"
    );
    assert!(
        quarantine_root(&b)
            .join("dropped")
            .join(format!("{id}.json"))
            .exists()
    );
}

#[test]
fn refile_of_a_letter_the_parser_still_refuses_keeps_it_held() {
    let (.., b) = pair();
    let bytes = newer_member_need("mind@00112233.local", &b.address("mind"), "future need");
    let id = sha256_hex(&bytes);
    let held = receipt("msg_0000000000000000000000dd", 7, "unknown field `flavor`");
    hold(&b.mailspace.dir, &bytes, &held).unwrap();

    let outcome = refile(&b.mailspace, &b.settings, &id).unwrap();

    let RefileOutcome::StillHeld(reason) = outcome else {
        panic!("the letter must stay held");
    };
    assert!(reason.contains("flavor"), "{reason}");
    assert_eq!(list(&b.mailspace.dir).unwrap().len(), 1, "still held");
    assert_eq!(live_count(&b, &id), 0, "nothing was filed");
}
