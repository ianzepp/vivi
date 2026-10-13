use crate::b64;
use crate::storage::{
    MessageIngestRequest, RemoteBindingInput, StoredMessageView, is_traveling_id, sha256_hex,
};
use crate::sync::fake_server::{Checkout, FakeServer, PROJECT, checkout, wire_id};
use crate::sync::link;
use crate::sync::state::SyncState;
use crate::sync::sync_dir;
use crate::sync::transport::{
    AppendFailure, Appended, Envelope, ListQuery, Page, Record, Transport,
};
use crate::{VivariumError, sync::pull};

const ID_A: &str = "msg_0000000000000000000000a1";
const ID_B: &str = "msg_0000000000000000000000b2";
const ID_C: &str = "msg_0000000000000000000000c3";

/// A mailspace elsewhere: its roles' addresses carry this domain.
const ELSEWHERE: &str = "a1b2c3d4.local";

fn eml(to: &str, subject: &str) -> Vec<u8> {
    eml_from(&format!("alice@{ELSEWHERE}"), to, subject)
}

fn eml_from(from: &str, to: &str, subject: &str) -> Vec<u8> {
    format!(
        "From: {from}\r\nTo: {to}\r\nSubject: {subject}\r\n\
         Message-ID: <{subject}@x.test>\r\nDate: Sun, 11 Oct 2026 12:00:00 +0000\r\n\r\nbody\r\n"
    )
    .into_bytes()
}

/// A record whose envelope `to` is the letter's own To and Cc, as a sender's
/// client declares it, stamped as an honest sender: with the prefix its From
/// address names, when it names one.
fn record(sequence: u64, id: &str, from: &str, kind: Option<&str>, bytes: &[u8]) -> Record {
    let headers = link::parse_headers(bytes);
    let to = headers.envelope_to();
    let from_prefix = headers
        .from
        .as_deref()
        .and_then(|address| address.rsplit_once('@'))
        .and_then(|(_, domain)| crate::mailspace::prefix_of_domain(domain))
        .map(String::from);
    Record {
        from_prefix,
        sequence,
        message_id: id.into(),
        from: from.into(),
        to: (!to.is_empty()).then_some(to),
        kind: kind.map(String::from),
        content_sha256: sha256_hex(bytes),
        appended_at: None,
        raw_eml: b64::encode(bytes),
    }
}

/// A transport that serves fixed pages and refuses appends.
struct Canned {
    pages: Vec<Page>,
}

impl Transport for Canned {
    fn append(&self, _: &str, _: &Envelope<'_>) -> Result<Appended, AppendFailure> {
        Err(AppendFailure::Stop("canned".into()))
    }

    fn fetch(&self, query: &ListQuery<'_>) -> Result<Page, VivariumError> {
        Ok(self
            .pages
            .iter()
            .find(|page| {
                page.records
                    .first()
                    .is_some_and(|r| r.sequence > query.since)
            })
            .cloned()
            .unwrap_or(Page {
                records: Vec::new(),
                next_cursor: query.since,
                has_more: None,
                epoch: None,
                epoch_unusable: false,
            }))
    }
}

fn pull_canned(fx: &Checkout, records: Vec<Record>) -> pull::PullReport {
    let next_cursor = records.last().map_or(0, |r| r.sequence);
    let canned = Canned {
        pages: vec![Page {
            records,
            next_cursor,
            has_more: None,
            epoch: None,
            epoch_unusable: false,
        }],
    };
    pull::pull(&fx.mailspace, &fx.settings, &canned).unwrap()
}

fn bob() -> Checkout {
    checkout("bob", &["mind", "hand"])
}

#[test]
fn the_summary_names_roles_counts_links_refusals_and_the_cursor() {
    let fx = bob();
    let to = fx.address("mind");
    let records = vec![
        record(1, ID_A, "alice", Some("need"), &eml(&to, "need-one")),
        record(2, ID_B, "alice", Some("mail"), &eml(&to, "note-one")),
        record(3, "legacy-id", "alice", None, &eml(&to, "legacy")),
    ];

    let report = pull_canned(&fx, records);
    let summary = report.summary();

    assert_eq!(
        summary,
        "sync pull: 3 fetched, 2 new (1 inbox, 1 needs), 0 already present, 0 reply link(s), \
         1 refused: legacy-id, cursor 3"
    );
}

#[test]
fn letters_file_by_the_envelope_kind_and_the_wire_id_is_kept() {
    let fx = bob();
    let to = fx.address("mind");
    let records = vec![
        record(1, ID_A, "alice", Some("task"), &eml(&to, "task-one")),
        record(2, ID_B, "alice", Some("want"), &eml(&to, "want-one")),
        record(3, ID_C, "alice", Some("novel"), &eml(&to, "odd")),
    ];

    let report = pull_canned(&fx, records);

    assert_eq!(report.filed.get("tasks"), Some(&1));
    assert_eq!(report.filed.get("wants"), Some(&1));
    assert_eq!(
        report.filed.get("inbox"),
        Some(&1),
        "unknown kinds are mail"
    );
    for id in [ID_A, ID_B] {
        let row = fx.row(id).unwrap();
        assert_eq!(row.account, "mind");
        assert!(is_traveling_id(&row.message_id));
    }
    let events = fx
        .mailspace
        .storage()
        .unwrap()
        .list_mailspace_events(ID_A)
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].command, "sync pull");
    assert_eq!(events[0].event_type, "delivered");
}

#[test]
fn a_letter_from_a_local_role_to_a_remote_one_files_a_read_sent_copy() {
    let fx = bob();
    let from = fx.address("hand");
    let to = format!("mind@{ELSEWHERE}");

    let report = pull_canned(
        &fx,
        vec![record(
            1,
            ID_A,
            "bob",
            Some("need"),
            &eml_from(&from, &to, "mine"),
        )],
    );

    assert_eq!(report.filed.get("sent"), Some(&1));
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let row = fx.row(ID_A).unwrap();
    assert_eq!(row.local_role, "sent");
    assert_eq!(row.account, "hand", "the From address's local part");
    assert!(row.read_state);
    assert!(
        fx.mailspace.backlog_item_tracked(&row.handle).unwrap(),
        "a work item sent to a remote address is tracked on its sent copy, as at the send"
    );
    assert_eq!(fx.rows().len(), 1);
}

#[test]
fn a_local_to_local_letter_with_nothing_held_files_both_rows() {
    let fx = bob();
    let (from, to) = (fx.address("hand"), fx.address("mind"));
    let bytes = eml_from(&from, &to, "restore me");

    let report = pull_canned(
        &fx,
        vec![record(1, ID_A, "someone-else", Some("need"), &bytes)],
    );

    assert_eq!(report.filed.get("needs"), Some(&1), "{:?}", report.filed);
    assert_eq!(report.filed.get("sent"), Some(&1), "{:?}", report.filed);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let delivered = fx.row(ID_A).expect("the wire id names the delivered row");
    assert_eq!(
        (delivered.account.as_str(), delivered.local_role.as_str()),
        ("mind", "needs")
    );
    assert!(!delivered.read_state);
    let rows = fx.rows();
    assert_eq!(rows.len(), 2);
    let sent = rows.iter().find(|row| row.local_role == "sent").unwrap();
    assert_eq!(sent.account, "hand");
    assert!(sent.read_state);
    assert_eq!(sent.content_id, delivered.content_id);
    assert!(delivered.remote.is_some() && sent.remote.is_some());
    assert!(
        fx.mailspace
            .backlog_item_tracked(&delivered.handle)
            .unwrap()
    );
    assert!(!fx.mailspace.backlog_item_tracked(&sent.handle).unwrap());
    let audit = fx.mailspace.backlog_audit(false).unwrap();
    assert!(audit.findings.is_empty(), "{:?}", audit.findings);
}

#[test]
fn a_letter_to_oneself_binds_only_the_delivered_row() {
    let fx = bob();
    let own = fx.address("mind");
    let bytes = eml_from(&own, &own, "note to self");

    let report = pull_canned(&fx, vec![record(1, ID_A, "bob", Some("mail"), &bytes)]);

    assert_eq!(report.filed.get("inbox"), Some(&1));
    assert_eq!(report.filed.get("sent"), Some(&1));
    assert!(report.warnings.is_empty());
    let rows = fx.rows();
    assert_eq!(rows.len(), 2);
    let delivered = rows.iter().find(|row| row.local_role == "inbox").unwrap();
    let sent = rows.iter().find(|row| row.local_role == "sent").unwrap();
    assert!(delivered.remote.is_some());
    assert!(
        sent.remote.is_none(),
        "one binding per account and position"
    );
    let again = pull_canned(&fx, vec![record(2, ID_B, "bob", Some("mail"), &bytes)]);
    assert_eq!(again.existing, 1);
    assert_eq!(fx.rows().len(), 2);
}

#[test]
fn a_letter_from_another_prefix_to_a_local_role_files_delivered_only() {
    let fx = bob();
    let bytes = eml_from("alice@9f8e7d6c.local", &fx.address("mind"), "from afar");

    let report = pull_canned(&fx, vec![record(1, ID_A, "alice", Some("mail"), &bytes)]);

    assert_eq!(report.filed.get("inbox"), Some(&1));
    assert_eq!(report.filed.get("sent"), None);
    assert_eq!(fx.rows().len(), 1);
}

#[test]
fn a_letter_files_once_per_local_role_it_names_and_not_for_other_mailspaces() {
    let fx = bob();
    let to = format!(
        "{}, Hand@{}, mind@{}, other@{ELSEWHERE}",
        fx.address("mind"),
        fx.mailspace.address_domain().to_ascii_uppercase(),
        fx.mailspace.legacy_domain()
    );

    let report = pull_canned(
        &fx,
        vec![record(
            1,
            ID_A,
            "alice",
            Some("need"),
            &eml(&to, "two roles"),
        )],
    );

    assert_eq!(report.filed.get("needs"), Some(&2), "{:?}", report.filed);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let rows = fx.rows();
    assert_eq!(rows.len(), 2);
    let mut accounts: Vec<&str> = rows.iter().map(|row| row.account.as_str()).collect();
    accounts.sort_unstable();
    assert_eq!(accounts, ["hand", "mind"]);
    assert_eq!(rows[0].content_id, rows[1].content_id);
    let named = fx.row(ID_A).expect("the wire id names the first row");
    assert!(is_traveling_id(&named.message_id));
    for row in &rows {
        let binding = row.remote.as_ref().expect("every row is bound");
        assert_eq!(binding.remote_uid, 1);
        assert!(fx.mailspace.backlog_item_tracked(&row.handle).unwrap());
    }

    // Replays change nothing, and finishing one copy completes its siblings.
    let again = pull_canned(
        &fx,
        vec![record(
            2,
            ID_B,
            "alice",
            Some("need"),
            &eml(&to, "two roles"),
        )],
    );
    assert_eq!(again.existing, 1);
    assert!(again.filed.is_empty());
    assert_eq!(fx.rows(), rows);
    let audit = fx.mailspace.backlog_audit(false).unwrap();
    assert!(audit.findings.is_empty(), "{:?}", audit.findings);
    fx.mailspace
        .move_item("mind", &named.handle, "done", None, "need done", None)
        .unwrap();
    let audit = fx.mailspace.backlog_audit(false).unwrap();
    assert!(audit.findings.is_empty(), "{:?}", audit.findings);
}

#[test]
fn a_role_this_mailspace_does_not_have_is_filed_under_its_name_and_warned_about() {
    let fx = bob();
    let to = format!("ghost@{}", fx.mailspace.address_domain());

    let report = pull_canned(
        &fx,
        vec![
            record(1, ID_A, "alice", Some("need"), &eml(&to, "for a ghost")),
            record(2, ID_B, "alice", Some("mail"), &eml(&to, "ghost mail")),
        ],
    );

    assert_eq!(report.filed.get("needs"), Some(&1));
    assert_eq!(report.filed.get("inbox"), Some(&1));
    assert_eq!(
        report.warnings,
        ["filed for unknown role ghost"],
        "once per name"
    );
    assert_eq!(fx.row(ID_A).unwrap().account, "ghost");
    assert!(report.failure().is_none(), "a warning is not a failure");
}

#[test]
fn a_letter_naming_no_role_here_is_held_under_its_first_to_address() {
    let fx = bob();
    let first = format!("hand@{ELSEWHERE}");
    let to = format!("{first}, mind@9f8e7d6c.local");

    let report = pull_canned(
        &fx,
        vec![record(
            1,
            ID_A,
            "alice",
            Some("need"),
            &eml(&to, "not ours"),
        )],
    );

    assert_eq!(report.filed.get("needs"), Some(&1));
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let row = fx.row(ID_A).unwrap();
    assert_eq!(row.account, first);
    assert_eq!(row.local_role, "needs");
    assert!(row.remote.is_some());
    assert!(
        !fx.mailspace.backlog_item_tracked(&row.handle).unwrap(),
        "another mailspace's work is held, not backlog"
    );
}

/// A vouch letter as the vouch verb writes it: From the sender's own member
/// name, To `postmaster` in its own domain, envelope `postmaster`.
fn vouch(fx: &Checkout, sequence: u64, id: &str) -> Record {
    let domain = fx.mailspace.legacy_domain();
    let bytes = eml_from(
        &format!("burgus@{domain}"),
        &format!("postmaster@{domain}"),
        "vouch pharos",
    );
    let mut record = record(sequence, id, "burgus", Some("mail"), &bytes);
    record.to = Some("postmaster".into());
    record
}

#[test]
fn a_vouch_letter_to_postmaster_is_governance_and_files_nothing() {
    let fx = bob();

    let report = pull_canned(&fx, vec![vouch(&fx, 1, ID_A)]);

    assert!(report.refused.is_empty(), "{:?}", report.refused);
    assert_eq!(
        report.governance, 1,
        "counted, not silent: {:?}",
        report.filed
    );
    assert!(report.filed.is_empty(), "{:?}", report.filed);
    assert!(
        report.unknown_roles.is_empty(),
        "{:?}",
        report.unknown_roles
    );
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert!(fx.rows().is_empty(), "no row at all: {:?}", fx.rows());
    let summary = report.summary();
    assert!(summary.contains("1 governance letter(s)"), "{summary}");
    assert!(!summary.contains("held for unknown role(s)"), "{summary}");

    // Case aside, and by the To header alone: any domain names governance.
    let bytes = eml_from(
        "alice@example.com",
        "POSTMASTER@Example.ORG",
        "to the office",
    );
    let report = pull_canned(&fx, vec![record(2, ID_B, "alice", Some("mail"), &bytes)]);
    assert_eq!(report.governance, 1);
    assert!(
        report.unknown_roles.is_empty(),
        "{:?}",
        report.unknown_roles
    );
    assert!(fx.rows().is_empty(), "{:?}", fx.rows());
}

#[test]
fn the_cursor_moves_past_a_governance_letter_and_a_second_pull_fetches_nothing() {
    let fx = bob();

    let report = pull_canned(&fx, vec![vouch(&fx, 4, ID_A)]);
    assert_eq!(report.cursor, 4, "the cursor advanced past it");
    let state = SyncState::load(&sync_dir(&fx.mailspace.dir), PROJECT).unwrap();
    assert_eq!(state.since, 4);

    let second = pull_canned(&fx, Vec::new());
    assert_eq!(second.fetched, 0, "a second pull fetches nothing new");
    assert_eq!(second.governance, 0);
    assert_eq!(second.cursor, 4);
    assert!(fx.rows().is_empty());
}

/// A pulled world with one need for a local role, one for another
/// mailspace's role, and a governance letter no role files.
fn foreign_world() -> (Checkout, StoredMessageView, StoredMessageView) {
    let fx = bob();
    let letters = vec![
        record(
            1,
            &wire_id(1, 0),
            "alice",
            Some("need"),
            &eml(&fx.address("hand"), "ours"),
        ),
        record(
            2,
            &wire_id(2, 0),
            "alice",
            Some("need"),
            &eml(&format!("hand@{ELSEWHERE}"), "not ours"),
        ),
        record(
            3,
            &wire_id(3, 0),
            "alice",
            Some("mail"),
            &eml("postmaster@audit.local", "vouch carol"),
        ),
    ];
    let report = pull_canned(&fx, letters);
    assert!(report.refused.is_empty(), "{:?}", report.refused);
    let (ours, theirs) = (
        fx.row(&wire_id(1, 0)).unwrap(),
        fx.row(&wire_id(2, 0)).unwrap(),
    );
    (fx, ours, theirs)
}

#[test]
fn work_held_for_another_mailspace_reaches_no_step_boot_or_audit_repair() {
    let (fx, ours, theirs) = foreign_world();

    let nodes = fx.mailspace.graph_show("backlog").unwrap().nodes;
    assert_eq!(nodes.len(), 1, "{nodes:?}");
    assert_eq!(nodes[0].source_id, ours.handle);
    let manifest = fx.mailspace.step_shadow().unwrap();
    let items: Vec<&str> = manifest
        .dispatches
        .iter()
        .map(|d| d.item.as_str())
        .chain(manifest.exceptions.iter().map(|e| e.item.as_str()))
        .collect();
    assert_eq!(items, [ours.handle.as_str()]);
    assert_eq!(
        fx.row(&wire_id(2, 0)).unwrap().account,
        format!("hand@{ELSEWHERE}"),
        "the row is still held"
    );

    let boot = crate::boot::BootReport::collect(&fx.mailspace).unwrap();
    let handles: Vec<&str> = boot.handles.iter().map(|h| h.handle.as_str()).collect();
    assert_eq!(handles, [ours.handle.as_str()]);
    assert!(boot.unabsorbed.is_empty(), "{:?}", boot.unabsorbed);

    let audit = fx.mailspace.backlog_audit(true).unwrap();
    assert!(audit.findings.is_empty(), "{:?}", audit.findings);
    assert!(!fx.mailspace.backlog_item_tracked(&theirs.handle).unwrap());
}

#[test]
fn a_node_minted_for_foreign_work_is_hidden_reported_and_settled_by_repair() {
    let (fx, ours, theirs) = foreign_world();
    // What an older binary left behind: a node for the foreign row.
    fx.mailspace
        .backlog_attach("need", &theirs.handle, "not ours", &[])
        .unwrap();

    let show = fx.mailspace.graph_show("backlog").unwrap();
    let ready: Vec<&str> = show.ready.iter().map(|n| n.source_id.as_str()).collect();
    assert_eq!(ready, [ours.handle.as_str()]);
    assert!(show.blocked.is_empty());
    let manifest = fx.mailspace.step_shadow().unwrap();
    assert!(
        manifest
            .dispatches
            .iter()
            .map(|d| &d.item)
            .chain(manifest.exceptions.iter().map(|e| &e.item))
            .all(|item| *item == ours.handle),
        "{manifest:?}"
    );

    let dry = fx.mailspace.backlog_audit(false).unwrap();
    assert_eq!(dry.findings.len(), 1, "{:?}", dry.findings);
    assert_eq!(dry.findings[0].class, "foreign_held");
    assert_eq!(dry.findings[0].handle, theirs.handle);
    assert!(!dry.findings[0].repaired);
    assert!(
        fx.mailspace
            .graph_show("backlog")
            .unwrap()
            .nodes
            .iter()
            .any(|n| n.source_id == theirs.handle && n.state == "open")
    );

    let repaired = fx.mailspace.backlog_audit(true).unwrap();
    assert!(repaired.findings[0].repaired);
    let node = fx
        .mailspace
        .graph_show("backlog")
        .unwrap()
        .nodes
        .into_iter()
        .find(|n| n.source_id == theirs.handle)
        .unwrap();
    assert_eq!(node.state, "done");
    assert_eq!(
        fx.row(&wire_id(2, 0)).unwrap().local_role,
        "needs",
        "the row is left"
    );
    assert!(
        fx.mailspace
            .backlog_audit(true)
            .unwrap()
            .findings
            .is_empty(),
        "folder wins must not reopen a settled foreign node"
    );
    assert_eq!(
        fx.mailspace
            .graph_show("backlog")
            .unwrap()
            .nodes
            .iter()
            .find(|n| n.source_id == theirs.handle)
            .unwrap()
            .state,
        "done"
    );
}

#[test]
fn an_envelope_that_disagrees_with_the_letter_is_refused() {
    let fx = bob();
    let to = fx.address("mind");
    let mut lying = record(1, ID_A, "alice", Some("mail"), &eml(&to, "lying"));
    lying.to = Some(format!("hand@{}", fx.mailspace.address_domain()));
    let mut partial = record(2, ID_B, "alice", Some("mail"), &eml(&to, "partial"));
    partial.to = Some(format!("{to},extra@{ELSEWHERE}"));

    let report = pull_canned(&fx, vec![lying, partial]);

    assert_eq!(report.refused.len(), 2);
    for refused in &report.refused {
        assert_eq!(
            refused.reason,
            "envelope recipients do not match the letter's To/Cc"
        );
    }
    assert!(report.failure().is_some());
    assert!(fx.rows().is_empty(), "nothing was filed");
}

#[test]
fn envelope_case_order_duplicates_absence_and_postmaster_are_not_mismatches() {
    let fx = bob();
    let (mind, hand) = (fx.address("mind"), fx.address("hand"));
    let both = eml(&format!("{mind}, {hand}"), "both");
    let mut shuffled = record(1, ID_A, "alice", Some("mail"), &both);
    shuffled.to = Some(format!("{}, {hand},{mind}", hand.to_ascii_uppercase()));
    let mut absent = record(2, ID_B, "alice", Some("mail"), &eml(&mind, "absent"));
    absent.to = None;
    let mut governance = record(3, ID_C, "alice", Some("mail"), &eml(&mind, "governance"));
    governance.to = Some("postmaster".into());

    let report = pull_canned(&fx, vec![shuffled, absent, governance]);

    assert!(report.refused.is_empty(), "{:?}", report.refused);
    assert_eq!(report.filed.get("inbox"), Some(&3), "{:?}", report.filed);
    assert_eq!(
        report.governance, 1,
        "the postmaster envelope is governance"
    );
}

#[test]
fn a_letter_to_the_legacy_domain_still_files_under_its_role() {
    let fx = bob();
    let to = format!("mind@{}", fx.mailspace.legacy_domain());

    let report = pull_canned(
        &fx,
        vec![record(
            1,
            ID_A,
            "alice",
            Some("need"),
            &eml(&to, "old school"),
        )],
    );

    assert!(report.refused.is_empty());
    assert_eq!(report.filed.get("needs"), Some(&1));
    assert_eq!(fx.row(ID_A).unwrap().account, "mind");
}

#[test]
fn a_renamed_role_receives_letters_addressed_to_its_old_name() {
    let mut fx = bob();
    fx.mailspace.rename_identity("hand", "arm").unwrap();
    let to = format!("hand@{}", fx.mailspace.address_domain());

    let report = pull_canned(
        &fx,
        vec![record(
            1,
            ID_A,
            "alice",
            Some("mail"),
            &eml(&to, "to the old name"),
        )],
    );

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(fx.row(ID_A).unwrap().account, "arm");
}

#[test]
fn content_already_held_under_another_id_is_not_filed_twice() {
    let fx = bob();
    let bytes = eml(&fx.address("mind"), "same-bytes");
    pull_canned(&fx, vec![record(1, ID_A, "alice", Some("need"), &bytes)]);

    let report = pull_canned(&fx, vec![record(2, ID_B, "alice", Some("need"), &bytes)]);

    assert_eq!(report.existing, 1);
    assert!(report.filed.is_empty());
    assert!(fx.row(ID_B).is_none());
    assert_eq!(fx.rows().len(), 1);
}

#[test]
fn an_id_held_with_different_bytes_is_refused_not_swapped_in() {
    let fx = bob();
    let to = fx.address("mind");
    let original = eml(&to, "original");
    pull_canned(&fx, vec![record(1, ID_A, "alice", Some("need"), &original)]);

    let report = pull_canned(
        &fx,
        vec![record(
            2,
            ID_A,
            "mallory",
            Some("need"),
            &eml(&to, "different"),
        )],
    );

    assert_eq!(report.refused.len(), 1);
    assert_eq!(report.refused[0].message_id, ID_A);
    assert_eq!(fx.rows().len(), 1);
    assert_eq!(fx.row(ID_A).unwrap().content_id, sha256_hex(&original));
}

#[test]
fn an_undecodable_record_is_refused() {
    let fx = bob();
    let mut bad = record(1, ID_A, "alice", None, &eml(&fx.address("mind"), "x"));
    bad.raw_eml = "!!!not base64".into();

    let report = pull_canned(&fx, vec![bad]);

    assert_eq!(report.refused.len(), 1);
    assert!(fx.rows().is_empty());
}

#[test]
fn the_cursor_is_stored_and_the_next_pull_resumes_after_it() {
    let server = FakeServer::new();
    let (a, b) = (checkout("alice", &["mind", "hand"]), bob());
    let (ca, cb) = (a.client(&server), b.client(&server));
    let to = b.address("mind");
    a.send_need("mind", &to, "first");
    a.send_need("mind", &to, "second");
    a.push(&ca);

    let first = b.pull(&cb);
    assert_eq!((first.fetched, first.cursor), (2, 2));
    let state = SyncState::load(&sync_dir(&b.mailspace.dir), PROJECT).unwrap();
    assert_eq!(state.since, 2);

    a.send_need("mind", &to, "third");
    a.push(&ca);
    let second = b.pull(&cb);
    assert_eq!((second.fetched, second.cursor), (1, 3));
    assert_eq!(second.existing, 0, "only the new record was read");

    let empty = b.pull(&cb);
    assert_eq!(
        (empty.fetched, empty.cursor),
        (0, 3),
        "an empty read keeps the cursor"
    );
}

#[test]
fn a_long_backlog_is_read_page_after_page() {
    let server = FakeServer::new();
    let (a, b) = (checkout("alice", &["mind", "hand"]), bob());
    let (ca, cb) = (a.client(&server), b.client(&server));
    for subject in ["one", "two", "three", "four", "five"] {
        a.send_need("mind", &b.address("mind"), subject);
    }
    a.push(&ca);
    server.page_limit(2);

    let report = b.pull(&cb);

    assert_eq!(report.fetched, 5);
    assert_eq!(report.filed.get("needs"), Some(&5));
    assert_eq!(report.cursor, 5);
}

#[test]
fn mail_for_another_member_is_not_even_served() {
    let server = FakeServer::new();
    let (a, b, c) = (
        checkout("alice", &["mind"]),
        bob(),
        checkout("carol", &["mind"]),
    );
    a.reply(
        &a.send_need("mind", &b.address("mind"), "parent"),
        "mind",
        &b.address("mind"),
        "private mail",
    );
    a.push(&a.client(&server));

    let report = c.pull(&c.client(&server));

    assert_eq!(report.fetched, 1, "the need, not the mail");
}

#[test]
fn a_failed_read_leaves_the_cursor_where_it_was() {
    struct Down;
    impl Transport for Down {
        fn append(&self, _: &str, _: &Envelope<'_>) -> Result<Appended, AppendFailure> {
            Err(AppendFailure::Stop("down".into()))
        }
        fn fetch(&self, _: &ListQuery<'_>) -> Result<Page, VivariumError> {
            Err(VivariumError::Message("project server unreachable".into()))
        }
    }
    let fx = bob();

    let error = pull::pull(&fx.mailspace, &fx.settings, &Down).unwrap_err();

    assert!(error.to_string().contains("unreachable"));
    assert!(!sync_dir(&fx.mailspace.dir).join("state.json").exists());
}

#[test]
fn a_row_bound_to_another_remote_is_left_alone_by_a_held_content_pull() {
    let fx = bob();
    let bytes = eml(&fx.address("mind"), "room-item");
    let mut storage = fx.mailspace.storage().unwrap();
    storage
        .ingest_message(
            &MessageIngestRequest {
                account: "mind".into(),
                local_role: "inbox".into(),
                read_state: false,
                starred: false,
                message_id_hint: None,
                presented_id: Some(ID_A.into()),
                seed_hint: "room".into(),
                remote: Some(RemoteBindingInput {
                    account: "mind".into(),
                    provider: "vivi-room".into(),
                    remote_mailbox: "room-x".into(),
                    remote_uid: 4,
                    remote_uidvalidity: 1,
                }),
            },
            &bytes,
        )
        .unwrap();
    drop(storage);

    let report = pull_canned(&fx, vec![record(9, ID_B, "alice", Some("mail"), &bytes)]);

    assert_eq!(report.existing, 1);
    let row: StoredMessageView = fx.row(ID_A).unwrap();
    assert_eq!(
        row.remote.unwrap().provider,
        "vivi-room",
        "its binding is kept"
    );
}

const CLAIM_REASON: &str = "From address claims mailspace";

/// A record of `bytes` stamped with `stamp` whatever its From says.
fn stamped(bytes: &[u8], stamp: Option<&str>) -> Record {
    let mut stamped = record(1, ID_A, "mallory", Some("mail"), bytes);
    stamped.from_prefix = stamp.map(String::from);
    stamped
}

#[test]
fn a_from_claiming_the_own_prefix_stamped_with_another_members_is_refused() {
    let fx = bob();
    let mine = fx.mailspace.address_domain();
    let forged = eml_from(&format!("hand@{mine}"), &fx.address("mind"), "forged");

    let report = pull_canned(&fx, vec![stamped(&forged, Some("deadbeef"))]);

    assert_eq!(report.refused.len(), 1);
    let reason = &report.refused[0].reason;
    assert!(reason.contains(CLAIM_REASON), "{reason}");
    assert!(reason.contains(fx.mailspace.address_prefix()), "{reason}");
    assert!(reason.contains("deadbeef"), "{reason}");
    assert!(report.failure().is_some());
    assert!(report.filed.is_empty(), "neither sent nor delivered");
    assert!(fx.rows().is_empty());
}

#[test]
fn a_from_claiming_a_remote_prefix_stamped_with_another_is_refused() {
    let fx = bob();
    let forged = eml_from("mind@9f8e7d6c.local", &fx.address("mind"), "forged");

    let report = pull_canned(&fx, vec![stamped(&forged, Some("a1b2c3d4"))]);

    assert_eq!(report.refused.len(), 1);
    assert!(report.refused[0].reason.contains("9f8e7d6c"));
    assert!(fx.rows().is_empty());
}

#[test]
fn a_prefix_shaped_from_with_no_stamp_is_refused_and_the_stamp_compares_case_blind() {
    let fx = bob();
    let bytes = eml_from("mind@9f8e7d6c.local", &fx.address("mind"), "unstamped");

    let report = pull_canned(&fx, vec![stamped(&bytes, None)]);

    assert_eq!(report.refused.len(), 1);
    assert!(report.refused[0].reason.ends_with("registered as none"));
    assert!(fx.rows().is_empty());

    let mut upper_record = stamped(&bytes, Some("9F8E7D6C"));
    upper_record.sequence = 2;
    let upper = pull_canned(&fx, vec![upper_record]);
    assert!(upper.refused.is_empty(), "{:?}", upper.refused);
    assert_eq!(fx.rows().len(), 1);
}

#[test]
fn from_addresses_that_claim_no_prefix_are_exempt_from_the_stamp() {
    let fx = bob();
    let to = fx.address("mind");
    let legacy = format!("mind@{}", fx.mailspace.legacy_domain());
    let bytes = [
        ("legacy", eml_from(&legacy, &to, "legacy-from")),
        ("external", eml_from("x@example.com", &to, "external-from")),
        ("short", eml_from("mind@3f9a1c7.local", &to, "short-prefix")),
        (
            "no from",
            format!("To: {to}\r\nSubject: anonymous\r\nMessage-ID: <anon@x>\r\n\r\nb\r\n")
                .into_bytes(),
        ),
    ];

    for (index, (what, bytes)) in bytes.iter().enumerate() {
        let id = format!("msg_0000000000000000000000d{index}");
        let mut candidate = record(index as u64 + 1, &id, "mallory", Some("mail"), bytes);
        candidate.from_prefix = Some("deadbeef".into());
        let report = pull_canned(&fx, vec![candidate]);
        assert!(report.refused.is_empty(), "{what}: {:?}", report.refused);
    }
    // The legacy-domain letter is From this mailspace too: delivered + sent.
    assert_eq!(fx.rows().len(), 5);
}

#[test]
fn a_slot_acting_as_this_mailspace_still_files_delivered_and_sent() {
    let fx = bob();
    let mine = fx.mailspace.address_prefix().to_string();
    let bytes = eml_from(&fx.address("hand"), &fx.address("mind"), "from the slot");

    let report = pull_canned(&fx, vec![stamped(&bytes, Some(&mine))]);

    assert!(report.refused.is_empty(), "{:?}", report.refused);
    assert_eq!(report.filed.get("inbox"), Some(&1));
    assert_eq!(report.filed.get("sent"), Some(&1));
}
