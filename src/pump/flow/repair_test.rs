//! Repair helpers: attempts, names, the record, attempt facts.

use tempfile::tempdir;

use super::{
    ATTEMPT_MARKER, ATTEMPT_SECS, AttemptFacts, attempt_of, branch, facts_text, fix_node,
    latest_facts, parse_facts, record, rung_node,
};
use crate::error::VivariumError;
use crate::mailspace::marked_line;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::flow::mail::MailWriter;
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;

const NEED: &str = "abcd1234";

fn facts(commit: &str) -> AttemptFacts {
    AttemptFacts {
        branch: branch(NEED, 1, 1),
        commit: commit.into(),
        base: "b".repeat(40),
    }
}

#[test]
fn attempts_come_from_the_node_name() {
    for (node, want) in [
        ("fix1", Some(1)),
        ("rung1", Some(1)),
        ("merge1", Some(1)),
        ("fix2", Some(2)),
        ("rung2", Some(2)),
        ("merge2", Some(2)),
        ("fix3", None),
        ("fix", None),
        ("sentinel", None),
        ("unrepaired", None),
    ] {
        assert_eq!(attempt_of(node), want, "{node}");
    }
    assert_eq!(fix_node(2), "fix2");
    assert_eq!(rung_node(1), "rung1");
    assert_eq!(ATTEMPT_SECS, 1800);
}

#[test]
fn the_branch_is_one_segment_after_the_prefix() {
    let name = branch(NEED, 1, 2);
    assert_eq!(name, "vivi/repair/abcd1234-r1-a2");
    assert!(!name.strip_prefix("vivi/repair/").unwrap().contains('/'));
}

#[test]
fn a_record_gives_target_tested_and_the_tail() -> R {
    let body =
        "kind: repair\ntarget: default\ntested: 1234abcd\n\nthread 'x' panicked\nFAILED a::b\n";
    let rec = record(body)?;
    assert_eq!(rec.target, "default");
    assert_eq!(rec.tested.as_deref(), Some("1234abcd"));
    assert_eq!(rec.tail, "thread 'x' panicked\nFAILED a::b");
    Ok(())
}

#[test]
fn tested_is_optional_and_target_is_required() -> R {
    let rec = record("kind: repair\ntarget: vivi\nboom\n")?;
    assert_eq!(rec.tested, None);
    assert_eq!(rec.tail, "boom");
    let err = record("kind: repair\ntested: abc\nboom\n").unwrap_err();
    assert!(matches!(err, VivariumError::Parse(text) if text.contains("target")));
    assert!(record("target:\n").is_err());
    Ok(())
}

#[test]
fn facts_round_trip_through_a_marked_line() {
    let want = facts(&"c".repeat(40));
    let text = facts_text(&want);
    assert_eq!(
        text,
        format!(
            "branch=vivi/repair/{NEED}-r1-a1 commit={} base={}",
            "c".repeat(40),
            "b".repeat(40)
        )
    );
    let stamped = marked_line(ATTEMPT_MARKER, &text);
    assert_eq!(parse_facts(&stamped), Some(want.clone()));
    assert_eq!(parse_facts(&format!("{ATTEMPT_MARKER} {text}")), Some(want));
    assert_eq!(parse_facts("pump-repair-attempt: branch=x commit=y"), None);
    assert_eq!(parse_facts("something else"), None);
}

fn step(node: &str, task: &str) -> StepRow {
    StepRow {
        run: 1,
        node: node.into(),
        task: task.into(),
        state: StepState::Done,
        verdict: None,
    }
}

fn stamped(commit: &str) -> String {
    marked_line(ATTEMPT_MARKER, &facts_text(&facts(commit)))
}

#[test]
fn latest_facts_picks_the_newest_fix_row_and_reply() -> R {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("pump");
    let old = mail.send_step("pump", "pump", "old", "body", &[])?;
    let new = mail.send_step("pump", "pump", "new", "body", &[])?;
    let other = mail.send_step("pump", "pump", "other", "body", &[])?;
    store.insert_step(NEED, &step("fix1", &old))?;
    store.insert_step(NEED, &step("fix1", &new))?;
    store.insert_step(NEED, &step("rung1", &other))?;
    mail.reply(&old, &stamped(&"1".repeat(40)))?;
    mail.reply(&new, "chatter")?;
    mail.reply(&new, &stamped(&"2".repeat(40)))?;
    mail.reply(&new, &marked_line(ATTEMPT_MARKER, "garbled"))?;
    let got = latest_facts(&store, &mail, "pump", NEED, 1, 1)?;
    assert_eq!(got.map(|f| f.commit), Some("2".repeat(40)));
    assert_eq!(latest_facts(&store, &mail, "pump", NEED, 1, 2)?, None);
    assert_eq!(latest_facts(&store, &mail, "pump", NEED, 2, 1)?, None);
    Ok(())
}

#[test]
fn latest_facts_reads_only_the_pump_identity() -> R {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("pump");
    let task = mail.send_step("pump", "pump", "fix", "body", &[])?;
    let only_foreign = mail.send_step("pump", "pump", "fix", "body", &[])?;
    store.insert_step(NEED, &step("fix1", &task))?;
    store.insert_step(NEED, &step("fix2", &only_foreign))?;
    mail.reply(&task, &stamped(&"1".repeat(40)))?;
    mail.foreign_reply(&task, "mallory", &stamped(&"9".repeat(40)));
    mail.foreign_reply(&only_foreign, "mallory", &stamped(&"9".repeat(40)));
    let got = latest_facts(&store, &mail, "pump", NEED, 1, 1)?;
    assert_eq!(got.map(|f| f.commit), Some("1".repeat(40)));
    assert_eq!(latest_facts(&store, &mail, "pump", NEED, 1, 2)?, None);
    Ok(())
}
