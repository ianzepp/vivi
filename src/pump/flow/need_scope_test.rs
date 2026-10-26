//! Tests for the typed scope rulings and the effective need scope readers.

use super::RulingDecision;
use super::ScopeIssue;
use super::ScopeRuling;
use super::decode;
use super::effective_need_scope;
use super::effective_target_scope;
use super::ruling_note_body;
use crate::letter::fixture::need_body_with;
use crate::letter::records::NoteWord;
use crate::letter::work::NeedFront;
use crate::mailspace::pump_note::note_body;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::slice_dispatch::SliceMail;

/// The pump's reply role: the only sender whose scope rulings count.
const REPLY_FROM: &str = "mind";

/// A need thread whose root letter carries `write_scope` and `target`.
fn a_need(mail: &mut FakeMail, write_scope: &[&str], target: Option<&str>) -> String {
    let front = NeedFront {
        write_scope: write_scope.iter().map(|path| (*path).to_string()).collect(),
        target: target.map(str::to_string),
        ..NeedFront::new(vec!["done".to_string()])
    };
    mail.send_step("mind", "hand", "need", &need_body_with(&front, ""), &[])
        .expect("need thread")
}

/// A valid grant ruling for `need`.
fn a_grant(need: &str, target: &str, paths: &[&str]) -> ScopeRuling {
    ScopeRuling {
        need: need.to_string(),
        target: target.to_string(),
        run: 2,
        task: "t0009".to_string(),
        paths: paths.iter().map(|path| (*path).to_string()).collect(),
        reason: "held by no open slice".to_string(),
        decision: RulingDecision::Grant,
        issue: None,
    }
}

/// Reply on `need` with the scope-ruling note for `ruling`, as the reply role.
fn grant_via_note(mail: &mut FakeMail, need: &str, ruling: &ScopeRuling, source_job: &str) {
    let body = ruling_note_body(ruling, source_job).expect("note body");
    mail.reply(need, &body).expect("reply");
}

#[test]
fn effective_need_scope_unions_root_and_grants() {
    let mut mail = FakeMail::new(REPLY_FROM);
    let need = a_need(&mut mail, &["src/pump/"], None);
    grant_via_note(
        &mut mail,
        &need,
        &a_grant(&need, "vivi", &["src/letter/records.rs"]),
        "job-1",
    );
    grant_via_note(
        &mut mail,
        &need,
        &a_grant(&need, "vivi", &["src/pump/", "docs/scope.md"]),
        "job-2-c2",
    );
    assert_eq!(
        effective_need_scope(&mail, &need, REPLY_FROM).expect("scope"),
        [
            "src/pump/".to_string(),
            "src/letter/records.rs".to_string(),
            "docs/scope.md".to_string()
        ]
    );
}

#[test]
fn effective_target_scope_keeps_each_repository_its_own() {
    let mut mail = FakeMail::new(REPLY_FROM);
    let need = a_need(&mut mail, &["src/"], Some("vivi"));
    grant_via_note(
        &mut mail,
        &need,
        &a_grant(&need, "vivi", &["src/pump/flow.rs"]),
        "job-1",
    );
    grant_via_note(
        &mut mail,
        &need,
        &a_grant(&need, "web", &["docs/home.md"]),
        "job-2",
    );
    assert_eq!(
        effective_target_scope(&mail, &need, "vivi", REPLY_FROM).expect("scope"),
        ["src/".to_string(), "src/pump/flow.rs".to_string()]
    );
    // The web grant never leaks into vivi, and vivi's own paths never leak
    // into the other repository.
    assert_eq!(
        effective_target_scope(&mail, &need, "web", REPLY_FROM).expect("scope"),
        Vec::<String>::new()
    );
}

#[test]
fn foreign_or_malformed_scope_note_grants_nothing() {
    let mut mail = FakeMail::new(REPLY_FROM);
    let need = a_need(&mut mail, &["src/"], None);

    // A valid grant from a foreign sender: notes_from keeps it out.
    let foreign = a_grant(&need, "vivi", &["docs/foreign.md"]);
    mail.foreign_reply(
        &need,
        "operator",
        &ruling_note_body(&foreign, "job-1").expect("note body"),
    );

    // Plain prose instead of the record.
    mail.reply(
        &need,
        &note_body(NoteWord::ScopeRuling, Some("job-2"), "please").expect("note body"),
    )
    .expect("reply");
    // A malformed record: unknown key.
    mail.reply(
        &need,
        &note_body(
            NoteWord::ScopeRuling,
            Some("job-3"),
            "need = \"x\"\nbogus = 1\n",
        )
        .expect("note body"),
    )
    .expect("reply");
    // An operator-required decision grants nothing even when well formed.
    let mut held = a_grant(&need, "vivi", &["docs/held.md"]);
    held.decision = RulingDecision::OperatorRequired;
    held.issue = Some(ScopeIssue::OutsideAllow);
    grant_via_note(&mut mail, &need, &held, "job-4");
    // A grant for another need.
    grant_via_note(
        &mut mail,
        &need,
        &a_grant("n999", "vivi", &["docs/other.md"]),
        "job-5",
    );
    // A record with no source reference in its ref.
    let unbacked = a_grant(&need, "vivi", &["docs/unbacked.md"]);
    mail.reply(
        &need,
        &note_body(
            NoteWord::ScopeRuling,
            None,
            &super::encode(&unbacked).expect("encode"),
        )
        .expect("note body"),
    )
    .expect("reply");

    assert_eq!(
        effective_need_scope(&mail, &need, REPLY_FROM).expect("scope"),
        ["src/".to_string()]
    );
}

#[test]
fn an_empty_original_write_scope_grants_nothing() {
    let mut mail = FakeMail::new(REPLY_FROM);
    let need = a_need(&mut mail, &[], None);
    grant_via_note(
        &mut mail,
        &need,
        &a_grant(&need, "vivi", &["docs/widened.md"]),
        "job-1",
    );
    assert!(
        effective_need_scope(&mail, &need, REPLY_FROM)
            .expect("scope")
            .is_empty()
    );
    assert!(
        effective_target_scope(&mail, &need, "vivi", REPLY_FROM)
            .expect("scope")
            .is_empty()
    );
}

#[test]
fn a_ruling_note_ref_names_the_base_job() {
    let mut mail = FakeMail::new(REPLY_FROM);
    let need = a_need(&mut mail, &["src/"], None);
    grant_via_note(
        &mut mail,
        &need,
        &a_grant(&need, "vivi", &["docs/a.md"]),
        "job-7-c2",
    );
    let notes = mail
        .notes_from(&need, NoteWord::ScopeRuling, REPLY_FROM)
        .expect("notes");
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].1.pump.reference.as_deref(), Some("job-7"));
    let ruling = decode(notes[0].1.prose.trim()).expect("record");
    assert_eq!(ruling, a_grant(&need, "vivi", &["docs/a.md"]));
}

#[test]
fn a_need_letter_that_does_not_read_is_an_error_not_empty_scope() {
    let mail = FakeMail::new(REPLY_FROM);
    assert!(effective_need_scope(&mail, "t404", REPLY_FROM).is_err());
}

#[test]
fn ruling_records_refuse_the_shaped_lies() {
    let base = a_grant("n1", "vivi", &["src/"]);
    let refused = |ruling: ScopeRuling, word: &str| {
        // Serialize raw: the decode side must catch what the encode side
        // already refuses.
        let text = toml::to_string(&ruling).expect("toml");
        let err = decode(&text).expect_err(word);
        assert!(err.to_string().contains(word), "{err}");
    };
    // A grant carrying an issue.
    let mut gated = base.clone();
    gated.issue = Some(ScopeIssue::Denied);
    refused(gated, "a grant carries no `issue`");
    // An operator-required decision without one.
    let mut ungated = base.clone();
    ungated.decision = RulingDecision::OperatorRequired;
    refused(ungated, "carries an `issue`");
    // A path that is not normalized.
    let mut raw = base.clone();
    raw.paths = vec!["./src/pump/".to_string()];
    refused(raw, "not normalized");
    // No paths at all.
    let mut bare = base.clone();
    bare.paths = Vec::new();
    refused(bare, "`paths` must not be empty");
    // Run zero.
    let mut unrun = base.clone();
    unrun.run = 0;
    refused(unrun, "`run` must be greater than zero");
    // A blank reason.
    let mut quiet = base;
    quiet.reason = "  ".to_string();
    refused(quiet, "`reason` must not be blank");
}
