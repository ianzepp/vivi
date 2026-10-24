//! The composed vouch letter: its headers and body, what it refuses before
//! any transport call, and the one governance letter `send_vouch` appends.

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::storage::is_traveling_id;
use crate::sync::fake_server::FakeServer;

use super::vouch::{VouchLetter, compose_vouch, send_vouch};

const DOMAIN: &str = "3f9a1c7e.local";
const GOOD_PEM: &str = "-----BEGIN PUBLIC KEY-----\nMCowBQYDK2VwAzEAgLVOmuQxwQH1SCgd/nsjXBTAKPTNMOiCOUVJ1tBMnNj1tw==\n-----END PUBLIC KEY-----";

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-11T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn composed() -> VouchLetter {
    compose_vouch("alice", DOMAIN, "bob", GOOD_PEM, now()).unwrap()
}

#[test]
fn the_letter_parses_back_to_the_member_and_key() {
    let letter = composed();
    let parsed = mail_parser::MessageParser::default()
        .parse(&letter.raw_eml)
        .unwrap();
    let from = parsed
        .from()
        .and_then(|a| a.first())
        .and_then(|a| a.address());
    assert_eq!(from, Some(format!("alice@{DOMAIN}").as_str()));
    let to = parsed
        .to()
        .and_then(|a| a.first())
        .and_then(|a| a.address());
    assert_eq!(to, Some(format!("postmaster@{DOMAIN}").as_str()));
    assert_eq!(parsed.subject(), Some("vouch bob"));
    assert!(parsed.message_id().is_some_and(|id| {
        let id = id.trim_matches(|c| c == '<' || c == '>');
        id.starts_with("vouch-bob-") && id.ends_with(&format!("@{DOMAIN}"))
    }));
    let date = parsed.date().unwrap().to_timestamp();
    assert_eq!(date, now().timestamp());

    let body: Value = serde_json::from_slice(parsed.body_text(0).unwrap().as_bytes()).unwrap();
    assert_eq!(body["vouch"]["name"], "bob");
    assert_eq!(body["vouch"]["public_key_pem"], GOOD_PEM);
}

#[test]
fn a_vouch_is_format_one_mail_and_of_message_reads_the_json_prose() {
    let letter = composed();
    let text = String::from_utf8(letter.raw_eml.clone()).unwrap();
    assert!(
        text.contains("X-Vivi-Format: 1\n"),
        "format header missing: {text}"
    );
    assert!(text.contains("X-Vivi-Kind: mail\n"), "{text}");

    let parsed = crate::letter::of_message(&letter.raw_eml, "vouch-bob").unwrap();
    assert_eq!(parsed.kind, crate::letter::LetterKind::Mail);
    assert!(matches!(parsed.front, crate::letter::Front::Mail(_)));
    assert!(
        !parsed.prose.starts_with("+++"),
        "the vouch fact is prose, not front matter: {}",
        parsed.prose
    );
    let body: Value = serde_json::from_str(&parsed.prose).unwrap();
    assert_eq!(body["vouch"]["name"], "bob");
    assert_eq!(body["vouch"]["public_key_pem"], GOOD_PEM);
}

#[test]
fn bad_names_and_bad_pems_are_refused_without_a_transport_call() {
    let server = FakeServer::new();
    for member in ["", " ", "bob carol", "bob\t", "\nbob"] {
        let letter = compose_vouch("alice", DOMAIN, member, GOOD_PEM, now());
        assert!(letter.is_err(), "{member:?} must be refused");
        assert!(
            letter.unwrap_err().to_string().contains("no whitespace"),
            "{member:?}"
        );
    }
    for pem in ["", "not a pem", "-----BEGIN CERTIFICATE-----"] {
        let letter = compose_vouch("alice", DOMAIN, "bob", pem, now());
        assert!(letter.is_err(), "{pem:?} must be refused");
        assert!(
            letter.unwrap_err().to_string().contains("public key PEM"),
            "{pem:?}"
        );
    }
    assert_eq!(server.append_calls(), 0, "nothing reached the transport");
}

#[test]
fn send_vouch_appends_one_governance_letter_to_the_postmaster() {
    let server = FakeServer::new();
    let client = server.client("alice", "3f9a1c7e");
    let letter = composed();

    let appended = send_vouch(&client, &letter).unwrap();

    assert!(is_traveling_id(&appended.message_id));
    assert!(!appended.duplicate);
    let items = server.items();
    assert_eq!(items.len(), 1, "one letter, nothing else");
    assert_eq!(items[0].to.as_deref(), Some("postmaster"));
    assert_eq!(items[0].kind.as_deref(), Some("mail"));
    assert_eq!(items[0].from, "alice");
    assert_eq!(items[0].message_id, letter.message_id);
    assert_eq!(items[0].bytes, letter.raw_eml, "byte-exact storage");
}

#[test]
fn refusals_and_stops_become_errors_with_the_server_reason() {
    let server = FakeServer::new();
    let client = server.client("alice", "3f9a1c7e");
    let refused = composed();
    server.refuse(&refused.message_id, 409);

    let error = send_vouch(&client, &refused).unwrap_err().to_string();
    assert!(error.contains("409"), "{error}");

    server.rate_limit_after(0);
    let stopped = composed();
    let error = send_vouch(&client, &stopped).unwrap_err().to_string();
    assert!(error.contains("429"), "{error}");
}

#[test]
fn a_replayed_vouch_comes_back_as_a_duplicate() {
    let server = FakeServer::new();
    let client = server.client("alice", "3f9a1c7e");
    let letter = composed();
    send_vouch(&client, &letter).unwrap();

    let replay = send_vouch(&client, &letter).unwrap();

    assert!(replay.duplicate);
    assert_eq!(replay.message_id, letter.message_id);
    assert_eq!(server.items().len(), 1);
}
