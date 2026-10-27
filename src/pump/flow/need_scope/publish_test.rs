//! Tests for the authorization operation: publishing a ruling before
//! permission takes effect, adopting a posted note on replay, and persisting
//! operator-required decisions.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use tempfile::tempdir;

use super::{ScopeAsk, ScopePublication, authorize_request};
use crate::error::VivariumError;
use crate::letter::fixture::need_body_with;
use crate::letter::records::NoteWord;
use crate::letter::work::NeedFront;
use crate::mailspace::pump_note::note_of;
use crate::pump::config::{PumpConfig, PumpTarget};
use crate::pump::flow::fake_mail::{Call, FakeMail};
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::need_scope::{
    RequestTarget, RulingDecision, ScopeContext, ScopeIssue, ScopeRequest, ScopeRuling, decode,
    effective_target_scope, ruling_note_body,
};
use crate::pump::store::PumpStore;

/// The pump's reply role: the only sender whose scope rulings count.
const REPLY_FROM: &str = "mind";
/// The target the requests name.
const TARGET: &str = "vivi";
/// The publication clock stamp the tests carry.
const NOW: u64 = 1_777_777_777;

/// A need thread whose root letter delegates `src/pump/`.
fn a_need(mail: &mut FakeMail, write_scope: &[&str]) -> String {
    let front = NeedFront {
        write_scope: write_scope.iter().map(|path| (*path).to_string()).collect(),
        ..NeedFront::new(vec!["done".to_string()])
    };
    mail.send_step("mind", "hand", "need", &need_body_with(&front, ""), &[])
        .expect("need thread")
}

/// A verifiable checkout with a real `src/` tree, including a denied
/// `src/secret/` directory.
fn a_checkout() -> tempfile::TempDir {
    let dir = tempdir().expect("temp checkout should create");
    fs::create_dir_all(dir.path().join("src/secret")).expect("tree should create");
    fs::write(dir.path().join("src/lib.rs"), "fn main() {}\n").expect("file should write");
    dir
}

/// One configured target: `allow` is the allow list and `deny` its deny
/// list; the checkout is the resolved integration checkout.
fn a_target(allow: &[&str], deny: &[&str], checkout: &Path) -> PumpTarget {
    PumpTarget {
        scope_allow: Some(allow.iter().map(|entry| String::from(*entry)).collect()),
        scope_deny: deny.iter().map(|entry| String::from(*entry)).collect(),
        integration_repo: Some(checkout.to_string_lossy().into_owned()),
        ..PumpTarget::default()
    }
}

/// A pump config with one named target.
fn a_config(target: &PumpTarget) -> PumpConfig {
    let targets = [(TARGET.to_string(), target.clone())]
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    PumpConfig {
        targets,
        ..PumpConfig::default()
    }
}

/// The classifier context the tests authorize in.
fn a_context<'a>(config: &'a PumpConfig, project: &'a Path) -> ScopeContext<'a> {
    ScopeContext {
        config,
        project,
        branch_target: None,
        reply_from: REPLY_FROM,
    }
}

/// A store over `pump.db` in its own directory, with the db path back.
fn a_store(dir: &Path) -> (PumpStore, PathBuf) {
    let db = dir.join("pump.db");
    let store = PumpStore::open(&db).expect("store should open");
    (store, db)
}

/// A role request of `need` asking for `paths`, keyed to `source`.
fn an_ask(need: &str, run: u32, task: &str, source: &str, paths: &[&str]) -> ScopeAsk {
    ScopeAsk {
        source: source.to_string(),
        request: ScopeRequest {
            need: need.to_string(),
            task: task.to_string(),
            run,
            target: RequestTarget::Named(TARGET.to_string()),
            paths: paths.iter().map(|path| String::from(*path)).collect(),
        },
        node: format!("slice:{run}"),
        attempt: 1,
        reason: "the slice needs the file".to_string(),
        design: false,
    }
}

/// Authorize `ask` at the fixed test clock.
fn authorize(
    store: &PumpStore,
    mail: &mut FakeMail,
    context: &ScopeContext<'_>,
    ask: &ScopeAsk,
) -> Result<ScopePublication, VivariumError> {
    authorize_request(store, mail, context, ask, NOW)
}

/// How many need-audience posts the fake recorded.
fn posted_count(mail: &FakeMail) -> usize {
    mail.calls
        .iter()
        .filter(|call| matches!(call, Call::PostToNeed { .. }))
        .count()
}

/// The body of the single need-audience post for `need`.
fn posted_body(mail: &FakeMail, need: &str) -> String {
    let bodies: Vec<String> = mail
        .calls
        .iter()
        .filter_map(|call| match call {
            Call::PostToNeed { need: to, body } if to == need => Some(body.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(bodies.len(), 1, "exactly one post expected");
    bodies[0].clone()
}

/// The test-only database fault of an interrupted run: forget that the
/// ruling was acknowledged, leaving the posted note unacknowledged.
fn forget_publication(db: &Path) {
    let conn = rusqlite::Connection::open(db).expect("fault connection should open");
    conn.execute("UPDATE scope_journal SET published_at = NULL", [])
        .expect("fault update should apply");
}

/// The `pump.ref` of a posted note body.
fn posted_ref(body: &str) -> String {
    let note = note_of(body).expect("posted note should parse");
    note.pump
        .reference
        .expect("posted note should carry a ref")
        .to_string()
}

/// A grant ruling for `need`, shaped like the one the classifier produces.
fn a_grant(need: &str, paths: &[&str]) -> ScopeRuling {
    ScopeRuling {
        need: need.to_string(),
        target: TARGET.to_string(),
        run: 2,
        task: "t0001".to_string(),
        paths: paths.iter().map(|path| (*path).to_string()).collect(),
        reason: "the slice needs the file".to_string(),
        decision: RulingDecision::Grant,
        issue: None,
    }
}

#[test]
fn scope_ruling_replay_posts_once() {
    let checkout = a_checkout();
    let config = a_config(&a_target(&["src/"], &[], checkout.path()));
    let work = tempdir().expect("work dir should create");
    let (store, db) = a_store(work.path());
    let mut mail = FakeMail::new(REPLY_FROM);
    let need = a_need(&mut mail, &["src/pump/"]);
    let context = a_context(&config, checkout.path());
    // A correction-suffixed source keys to its base everywhere.
    let ask = an_ask(&need, 2, "t0001", "job-9-c2", &["src/lib.rs"]);

    let first = authorize(&store, &mut mail, &context, &ask).expect("authorize");
    assert_eq!(first, ScopePublication::Published);
    assert_eq!(posted_count(&mail), 1);
    let body = posted_body(&mail, &need);
    assert_eq!(posted_ref(&body), "job-9");
    let row = store
        .scope_request("job-9-c2")
        .expect("row")
        .expect("prepared");
    assert_eq!(row.decision, RulingDecision::Grant);
    assert!(row.published_at.is_some());

    // A crash between post and acknowledgement: the journal lost its stamp
    // while the note stayed on the thread.
    forget_publication(&db);
    mail.reply(&need, &body)
        .expect("the interrupted note is on the thread");
    let row = store
        .scope_request("job-9-c2")
        .expect("row")
        .expect("prepared");
    assert!(row.published_at.is_none());

    // The replay adopts the posted note instead of posting again, and the
    // adopted grant is live permission evidence.
    let second = authorize(&store, &mut mail, &context, &ask).expect("authorize");
    assert_eq!(second, ScopePublication::Adopted);
    assert_eq!(posted_count(&mail), 1);
    let row = store
        .scope_request("job-9-c2")
        .expect("row")
        .expect("prepared");
    assert!(row.published_at.is_some());
    assert_eq!(
        effective_target_scope(&mail, &need, TARGET, REPLY_FROM).expect("scope"),
        ["src/pump/".to_string(), "src/lib.rs".to_string()]
    );

    // A later replay adopts again and still never reposts.
    let third = authorize(&store, &mut mail, &context, &ask).expect("authorize");
    assert_eq!(third, ScopePublication::Adopted);
    assert_eq!(posted_count(&mail), 1);
}

#[test]
fn scope_grant_survives_store_reopen_and_new_run() {
    let checkout = a_checkout();
    let generous = a_config(&a_target(&["src/"], &[], checkout.path()));
    let work = tempdir().expect("work dir should create");
    let (store, _db) = a_store(work.path());
    let mut mail = FakeMail::new(REPLY_FROM);
    let need = a_need(&mut mail, &["src/pump/"]);
    let ask = an_ask(&need, 2, "t0001", "job-9", &["src/lib.rs"]);

    let context = a_context(&generous, checkout.path());
    let first = authorize(&store, &mut mail, &context, &ask).expect("authorize");
    assert_eq!(first, ScopePublication::Published);
    assert_eq!(posted_count(&mail), 1);
    // The posted note lives on the need thread; the fake mirrors what the
    // port's audience post does to the thread.
    mail.reply(&need, &posted_body(&mail, &need))
        .expect("thread the note");

    // The store reopens under a reloaded, tighter policy: the granted path
    // sits outside the new allow list now.
    drop(store);
    let (store, _db) = a_store(work.path());
    let tight = a_config(&a_target(&["src/pump/"], &[], checkout.path()));
    let context = a_context(&tight, checkout.path());

    // The same source replays: the published decision stays authoritative,
    // no re-classification denies it, and no second post appears.
    let second = authorize(&store, &mut mail, &context, &ask).expect("authorize");
    assert_eq!(second, ScopePublication::Adopted);
    assert_eq!(posted_count(&mail), 1);
    let row = store
        .scope_request("job-9")
        .expect("row")
        .expect("prepared");
    assert_eq!(row.decision, RulingDecision::Grant);
    assert!(row.published_at.is_some());

    // A new run asks for the same path again: it is already held, so
    // nothing is posted and the standing grant stays the evidence.
    let new_run = an_ask(&need, 3, "t0002", "job-11", &["src/lib.rs"]);
    let third = authorize(&store, &mut mail, &context, &new_run).expect("authorize");
    assert_eq!(third, ScopePublication::Held);
    assert_eq!(posted_count(&mail), 1);
    assert_eq!(
        effective_target_scope(&mail, &need, TARGET, REPLY_FROM).expect("scope"),
        ["src/pump/".to_string(), "src/lib.rs".to_string()]
    );
}

#[test]
fn a_rejected_publication_errors_and_publishes_on_retry() {
    let checkout = a_checkout();
    let config = a_config(&a_target(&["src/"], &[], checkout.path()));
    let work = tempdir().expect("work dir should create");
    let (store, _db) = a_store(work.path());
    let mut mail = FakeMail::new(REPLY_FROM);
    let need = a_need(&mut mail, &["src/pump/"]);
    let context = a_context(&config, checkout.path());
    let ask = an_ask(&need, 2, "t0001", "job-9", &["src/lib.rs"]);

    // The post is rejected: the operation fails, the record stays
    // unpublished, and nothing is mutated or dispatched after it.
    let before = mail.calls.len();
    mail.fail_at = Some(before);
    assert!(authorize(&store, &mut mail, &context, &ask).is_err());
    assert_eq!(posted_count(&mail), 0);
    assert_eq!(mail.calls.len(), before);
    let row = store
        .scope_request("job-9")
        .expect("row")
        .expect("prepared");
    assert!(row.published_at.is_none());

    // The retry publishes the same prepared record exactly once.
    mail.fail_at = None;
    let retried = authorize(&store, &mut mail, &context, &ask).expect("authorize");
    assert_eq!(retried, ScopePublication::Published);
    assert_eq!(posted_count(&mail), 1);
    assert_eq!(mail.calls.len(), before + 1);
    let row = store
        .scope_request("job-9")
        .expect("row")
        .expect("prepared");
    assert_eq!(row.decision, RulingDecision::Grant);
    assert!(row.published_at.is_some());
    // The published grant is live permission evidence.
    mail.reply(&need, &posted_body(&mail, &need))
        .expect("thread the note");
    assert_eq!(
        effective_target_scope(&mail, &need, TARGET, REPLY_FROM).expect("scope"),
        ["src/pump/".to_string(), "src/lib.rs".to_string()]
    );
}

#[test]
fn a_denied_request_persists_its_issue_and_grants_nothing() {
    let checkout = a_checkout();
    let config = a_config(&a_target(&["src/"], &["src/secret/"], checkout.path()));
    let work = tempdir().expect("work dir should create");
    let (store, _db) = a_store(work.path());
    let mut mail = FakeMail::new(REPLY_FROM);
    let need = a_need(&mut mail, &["src/pump/"]);
    let context = a_context(&config, checkout.path());
    let ask = an_ask(&need, 2, "t0001", "job-9", &["src/secret/"]);

    let published = authorize(&store, &mut mail, &context, &ask).expect("authorize");
    assert_eq!(published, ScopePublication::Published);
    assert_eq!(posted_count(&mail), 1);

    // The journal carries the typed policy issue with no grant.
    let row = store
        .scope_request("job-9")
        .expect("row")
        .expect("prepared");
    assert_eq!(row.decision, RulingDecision::OperatorRequired);
    assert_eq!(row.issue, Some(ScopeIssue::Denied));
    assert!(row.published_at.is_some());

    // The posted note carries the operator-required record, not a grant.
    let note = note_of(&posted_body(&mail, &need)).expect("note");
    let record = decode(note.prose.trim()).expect("record");
    assert_eq!(record.decision, RulingDecision::OperatorRequired);
    assert_eq!(record.issue, Some(ScopeIssue::Denied));

    // The need's effective scope is unchanged: an operator-required ruling
    // grants nothing.
    assert_eq!(
        effective_target_scope(&mail, &need, TARGET, REPLY_FROM).expect("scope"),
        ["src/pump/".to_string()]
    );
}

#[test]
fn a_policy_change_redecides_an_unpublished_preparation() {
    let checkout = a_checkout();
    let generous = a_config(&a_target(&["src/"], &[], checkout.path()));
    let work = tempdir().expect("work dir should create");
    let (store, _db) = a_store(work.path());
    let mut mail = FakeMail::new(REPLY_FROM);
    let need = a_need(&mut mail, &["src/pump/"]);
    let ask = an_ask(&need, 2, "t0001", "job-9", &["src/lib.rs"]);

    // The preparation under the generous policy never gets its note out.
    let context = a_context(&generous, checkout.path());
    mail.fail_at = Some(mail.calls.len());
    assert!(authorize(&store, &mut mail, &context, &ask).is_err());
    let row = store
        .scope_request("job-9")
        .expect("row")
        .expect("prepared");
    assert_eq!(row.decision, RulingDecision::Grant);
    assert!(row.published_at.is_none());
    mail.fail_at = None;

    // The policy reloads tight before the retry: the unpublished request is
    // re-decided by current policy, refreshed in the journal, and the
    // operator-required ruling is what gets published.
    let tight = a_config(&a_target(&["src/pump/"], &[], checkout.path()));
    let context = a_context(&tight, checkout.path());
    let retried = authorize(&store, &mut mail, &context, &ask).expect("authorize");
    assert_eq!(retried, ScopePublication::Published);
    assert_eq!(posted_count(&mail), 1);
    let row = store
        .scope_request("job-9")
        .expect("row")
        .expect("prepared");
    assert_eq!(row.decision, RulingDecision::OperatorRequired);
    assert_eq!(row.issue, Some(ScopeIssue::OutsideAllow));
    assert!(row.published_at.is_some());
    let note = note_of(&posted_body(&mail, &need)).expect("note");
    let record = decode(note.prose.trim()).expect("record");
    assert_eq!(record.decision, RulingDecision::OperatorRequired);
    assert_eq!(
        effective_target_scope(&mail, &need, TARGET, REPLY_FROM).expect("scope"),
        ["src/pump/".to_string()]
    );
}

#[test]
fn a_forged_or_mismatched_note_is_never_acknowledgement() {
    let checkout = a_checkout();
    let config = a_config(&a_target(&["src/"], &[], checkout.path()));
    let work = tempdir().expect("work dir should create");
    let (store, _db) = a_store(work.path());
    let mut mail = FakeMail::new(REPLY_FROM);
    let need = a_need(&mut mail, &["src/pump/"]);
    let context = a_context(&config, checkout.path());
    let ask = an_ask(&need, 2, "t0001", "job-9", &["src/lib.rs"]);

    // Leave the request prepared but unpublished, with no note posted.
    mail.fail_at = Some(mail.calls.len());
    assert!(authorize(&store, &mut mail, &context, &ask).is_err());
    mail.fail_at = None;

    // A matching record from a foreign sender: notes_from keeps it out.
    let forged =
        ruling_note_body(&a_grant(&need, &["src/lib.rs"]), "job-9").expect("forged note body");
    mail.foreign_reply(&need, "operator", &forged);
    // A malformed record behind the right reference.
    mail.reply(
        &need,
        &crate::mailspace::pump_note::note_body(
            NoteWord::ScopeRuling,
            Some("job-9"),
            "please just allow it",
        )
        .expect("note body"),
    )
    .expect("reply");
    // A well-formed record from the reply role that decides other paths.
    let other = ruling_note_body(&a_grant(&need, &["docs/other.md"]), "job-9")
        .expect("mismatched note body");
    mail.reply(&need, &other).expect("reply");

    // None of them is acknowledgement: the operation publishes its own note.
    let published = authorize(&store, &mut mail, &context, &ask).expect("authorize");
    assert_eq!(published, ScopePublication::Published);
    assert_eq!(posted_count(&mail), 1);
    let row = store
        .scope_request("job-9")
        .expect("row")
        .expect("prepared");
    assert!(row.published_at.is_some());
}

#[test]
fn a_request_with_no_target_journals_without_posting() {
    let checkout = a_checkout();
    let config = a_config(&a_target(&["src/"], &[], checkout.path()));
    let work = tempdir().expect("work dir should create");
    let (store, _db) = a_store(work.path());
    let mut mail = FakeMail::new(REPLY_FROM);
    let need = a_need(&mut mail, &["src/pump/"]);
    let context = a_context(&config, checkout.path());
    let mut ask = an_ask(&need, 2, "t0001", "job-9", &["src/lib.rs"]);
    ask.request.target = RequestTarget::Named("   ".to_string());

    // A blank target cannot name a note: the typed record is journalled and
    // nothing is posted or granted.
    let settled = authorize(&store, &mut mail, &context, &ask).expect("authorize");
    assert_eq!(settled, ScopePublication::Journalled);
    assert_eq!(posted_count(&mail), 0);
    let row = store
        .scope_request("job-9")
        .expect("row")
        .expect("prepared");
    assert_eq!(row.decision, RulingDecision::OperatorRequired);
    assert_eq!(row.issue, Some(ScopeIssue::UnknownTarget));
    assert_eq!(row.target, "");
    assert!(row.published_at.is_none());
}
