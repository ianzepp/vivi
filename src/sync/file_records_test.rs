//! `file_records`: a host that receives letters directly (the server filing
//! a member's push) files them exactly as a pull files a page, with no
//! transport.

use std::rc::Rc;

use crate::b64;
use crate::mailspace::{AbsorbConfig, SendRequest};
use crate::sync::fake_server::{Checkout, FakeServer, Item, checkout};
use crate::sync::pull::{PullReport, file_records};
use crate::sync::state::SyncState;
use crate::sync::transport::Record;

fn alice() -> Checkout {
    let mut alice = checkout("alice", &["mind"]);
    alice.mailspace.config.absorb = Some(AbsorbConfig { quiet_hours: 0 });
    alice
}

/// The role-based mailspace the server hosts, with roles `mind` and `hand`.
fn hosted() -> Checkout {
    checkout("bob", &["mind", "hand"])
}

fn send_need(from: &Checkout, to: &str) -> String {
    let result = from
        .mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec![to.into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "ship it".into(),
            body: "done_when: it works\n".into(),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap();
    result
        .delivered
        .first()
        .map_or(result.sent, |delivered| delivered.handle.clone())
}

/// What a server holds for a pushed letter, as the wire record.
fn record_of(item: &Item) -> Record {
    Record {
        sequence: item.sequence,
        message_id: item.message_id.clone(),
        from: item.from.clone(),
        to: item.to.clone(),
        kind: item.kind.clone(),
        content_sha256: item.sha.clone(),
        from_prefix: item.from_prefix.clone(),
        appended_at: item.appended_at.clone(),
        raw_eml: b64::encode(&item.bytes),
    }
}

fn file(host: &Checkout, state: &mut SyncState, server: &Rc<FakeServer>) -> PullReport {
    let records: Vec<Record> = server.items().iter().map(record_of).collect();
    file_records(&host.mailspace, &host.settings, state, &records).unwrap()
}

fn roles_of(host: &Checkout) -> Vec<(String, String)> {
    let mut roles: Vec<(String, String)> = host
        .rows()
        .into_iter()
        .map(|row| (row.account, row.local_role))
        .collect();
    roles.sort();
    roles
}

/// Alice sends a need to the hosted mailspace's `hand` and pushes it.
fn pushed_need() -> (Rc<FakeServer>, Checkout, Checkout, String) {
    let server = FakeServer::new();
    let (alice, host) = (alice(), hosted());
    let need = send_need(&alice, &host.address("hand"));
    alice.push(&alice.client(&server));
    (server, alice, host, need)
}

#[test]
fn a_need_for_a_role_lands_in_that_roles_needs_folder() {
    let (server, _alice, host, _need) = pushed_need();
    let mut state = SyncState::default();

    let report = file(&host, &mut state, &server);

    assert_eq!(report.fetched, 1);
    assert_eq!(report.filed.get("needs"), Some(&1), "{report:?}");
    assert!(report.refused.is_empty(), "{:?}", report.refused);
    assert!(
        roles_of(&host).contains(&("hand".into(), "needs".into())),
        "{:?}",
        roles_of(&host)
    );
    assert_eq!(state.since, 0, "filing never moves a cursor");
}

#[test]
fn an_event_letter_folds_onto_its_item() {
    let (server, alice, host, need) = pushed_need();
    let mut state = SyncState::default();
    file(&host, &mut state, &server);

    alice
        .mailspace
        .move_task("mind", &need, "done", Some("accepted"), None, &[], &[])
        .unwrap();
    alice.push(&alice.client(&server));
    let report = file(&host, &mut state, &server);

    assert_eq!(report.events_applied, 1, "{report:?}");
    let row = host.row(&host.id_of(&need)).unwrap();
    assert_eq!(row.local_role, "done");
}

#[test]
fn filing_the_same_records_again_changes_nothing() {
    let (server, alice, host, need) = pushed_need();
    alice
        .mailspace
        .move_task("mind", &need, "done", None, None, &[], &[])
        .unwrap();
    alice.push(&alice.client(&server));
    let mut state = SyncState::default();
    file(&host, &mut state, &server);
    let before = roles_of(&host);

    let again = file(&host, &mut state, &server);

    assert!(again.filed.is_empty(), "{:?}", again.filed);
    assert_eq!(again.existing, 2);
    assert_eq!(roles_of(&host), before);
}

#[test]
fn a_record_whose_bytes_do_not_match_is_refused_not_filed() {
    let (server, _alice, host, _need) = pushed_need();
    let mut record = record_of(&server.items()[0]);
    record.content_sha256 = "0".repeat(64);

    let report = file_records(
        &host.mailspace,
        &host.settings,
        &mut SyncState::default(),
        &[record],
    )
    .unwrap();

    assert_eq!(report.refused.len(), 1);
    assert!(host.rows().is_empty());
}
