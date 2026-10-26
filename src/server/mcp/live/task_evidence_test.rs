//! Tests for [`super::task_evidence`]: ordering and paging thread messages
//! and newest-first attempts, malformed notes staying ordinary content, and
//! attempt bodies keeping the failure evidence.

use std::fmt::Write as _;

use super::{BODY_CAP, Window, task_evidence};
use crate::letter::records::{MailFront, NoteWord, PumpNote};
use crate::mailspace::MailspaceThreadMessage;

/// A stamp the two same-second notes share.
const T_SAME: i64 = 1_793_049_600_000_000;
/// An earlier stamp for the older attempt record.
const T_OLD: i64 = 1_793_049_000_000_000;

/// The newest attempt record: a failed hand whose excerpt carries the pool's
/// 429 rate-limit line.
const RECORD_429: &str = "\
pump: attempt record
attempt: 52a1b306-a2
need: abf4884a
task: 52a1b306
actor: 542e6330
role: hand
kind: none
pool: studio
slot: slot-15
model: zai/glm-5.3-flash
ready-at: none
start: 1793049839
end: 1793049856
agent-seconds: 17
delivered-at: none
merged-at: none
outcome: agent failed: hand failed
unit: 52a1b306  attempt: 2  model: zai/glm-5.3-flash  pool: studio  target: vivi
branch: vivi/unit/52a1b306-a2 (none: the hand left no changes)
task: 52a1b306
base:
failing command:
  agent exit 1
excerpt:
  429: {\"code\":\"1302\",\"message\":\"Rate limit reached for requests\"}
hand's final message:
  pool result 7516c305216a: finished
  pool studio  slot 542e6330-studio-slot-7  model zai/glm-5.3-flash
  agent exit 1 in 17s, job 19s, started from 1793049839
";

/// An older, successful attempt record.
const RECORD_DELIVERED: &str = "\
pump: attempt record
attempt: 52a1b306-a1
need: abf4884a
task: 52a1b306
pool: studio
start: 1793040000
end: 1793040120
agent-seconds: 120
outcome: delivered
hand's final message:
  delivered
";

/// A pool result reply, the receipt shape `parse_receipt` reads.
const RECEIPT: &str = "\
pool result j123: finished
pool studio  slot slot-15  model zai/glm-5.3-flash
agent exit 0 in 42s, job 90s, started from 1793040500";

/// One thread message with `handle`, `date` and `body`.
fn message(handle: &str, date: &str, body: &str) -> MailspaceThreadMessage {
    MailspaceThreadMessage {
        handle: handle.to_owned(),
        message_id: format!("mid-{handle}"),
        content_id: format!("cid-{handle}"),
        account: "studio".into(),
        role: "tasks".into(),
        kind: None,
        event: None,
        date: date.to_owned(),
        from: "mind@studio.local".into(),
        to: "hand@studio.local".into(),
        cc: String::new(),
        subject: "Extract, order, and bound recent task attempt evidence".into(),
        body: body.to_owned(),
        parent_content_id: None,
        link_source: None,
        inferred: false,
        marks: Vec::new(),
        close_branch: None,
        close_commit: None,
    }
}

/// A pump note carrying `prose`, stamped `t`.
fn stamped(t: i64, prose: &str) -> String {
    crate::letter::render(
        &MailFront {
            pump: Some(PumpNote {
                note: NoteWord::Attempt,
                t,
                reference: None,
            }),
            ..MailFront::default()
        },
        prose,
    )
    .unwrap()
}

/// One attempt record wrapped in a pump note stamped `t`.
fn record_note(handle: &str, date: &str, t: i64, record: &str) -> MailspaceThreadMessage {
    message(handle, date, &stamped(t, record))
}

/// One ordinary reply, `date` stamped, with no note at all.
fn reply(handle: &str, date: &str) -> MailspaceThreadMessage {
    message(handle, date, "an ordinary reply on the task thread")
}

#[test]
fn attempt_evidence_orders_and_pages() {
    let mut thread: Vec<MailspaceThreadMessage> = (0..22)
        .map(|n| {
            // r05 and r06 share a second; their ids keep r05 first.
            let minute = if n == 6 { 5 } else { n };
            reply(
                &format!("r{n:0>2}"),
                &format!("2026-10-26T21:{minute:0>2}:00Z"),
            )
        })
        .collect();
    thread.push(record_note(
        "old",
        "2026-10-26T21:30:00Z",
        T_OLD,
        RECORD_DELIVERED,
    ));
    thread.push(record_note(
        "new-a1",
        "2026-10-26T22:00:00Z",
        T_SAME,
        RECORD_DELIVERED,
    ));
    thread.push(record_note(
        "new-a2",
        "2026-10-26T22:00:00Z",
        T_SAME,
        RECORD_429,
    ));
    thread.push(message("pool", "2026-10-26T22:05:00Z", RECEIPT));

    let evidence = task_evidence(
        &thread,
        Window {
            offset: 0,
            limit: 10,
        },
        Window {
            offset: 0,
            limit: 2,
        },
    );
    assert_eq!(evidence.total, 26);
    assert_eq!(evidence.next_offset, Some(10));
    assert!(evidence.truncated);
    // Same-second replies keep id order; the page holds only ordinary rows.
    assert_eq!(evidence.messages[0].handle, "r00");
    assert_eq!(evidence.messages[5].handle, "r05");
    assert_eq!(evidence.messages[6].handle, "r06");
    assert!(!evidence.messages[0].is_attempt);
    // 22 older replies cannot hide the newest failure: the newest-first
    // attempts lead with the same-second pair, the 429 record first.
    assert_eq!(evidence.attempts_total, 4);
    assert_eq!(evidence.attempts[0].handle, "new-a2");
    assert_eq!(evidence.attempts[0].message_id, "mid-new-a2");
    assert_eq!(evidence.attempts[0].fields.attempt, "52a1b306-a2");
    assert_eq!(
        evidence.attempts[0].fields.outcome,
        "agent failed: hand failed"
    );
    assert_eq!(
        evidence.attempts[0].fields.model.as_deref(),
        Some("zai/glm-5.3-flash")
    );
    assert!(evidence.attempts[0].body.contains("1302"));
    // The same-second pair ties on stamp and date, so the id breaks it.
    assert_eq!(evidence.attempts[1].handle, "new-a1");
    assert_eq!(evidence.attempts_next_offset, Some(2));
    assert!(evidence.attempts_truncated);
    // The remaining attempts: the older delivered record, then the bare
    // pool receipt (unstamped letters sort first, so it runs last).
    let later = task_evidence(
        &thread,
        Window {
            offset: 0,
            limit: 1,
        },
        Window {
            offset: 2,
            limit: 10,
        },
    );
    assert_eq!(later.attempts[0].handle, "old");
    assert_eq!(later.attempts[0].fields.attempt, "52a1b306-a1");
    assert_eq!(later.attempts[0].fields.outcome, "delivered");
    // A bare receipt is an unstamped letter, so it sorts last.
    assert_eq!(later.attempts[1].handle, "pool");
    assert_eq!(later.attempts[1].fields.attempt, "j123");
    assert_eq!(later.attempts[1].fields.outcome, "finished");
    assert_eq!(later.attempts[1].fields.agent_seconds, Some(42));
    assert_eq!(later.attempts_next_offset, None);
    assert!(!later.attempts_truncated);
    // An offset at or past the end is the empty terminal page.
    let past = task_evidence(
        &thread,
        Window {
            offset: 26,
            limit: 5,
        },
        Window {
            offset: 4,
            limit: 5,
        },
    );
    assert!(past.messages.is_empty());
    assert_eq!(past.next_offset, None);
    assert!(!past.truncated);
    assert!(past.attempts.is_empty());
    assert_eq!(past.attempts_next_offset, None);
}

#[test]
fn malformed_attempt_notes_remain_content() {
    let thread = [
        // An attempt record missing its outcome: not a valid attempt note.
        message(
            "bad",
            "2026-10-26T21:00:00Z",
            &stamped(T_SAME, "pump: attempt record\nattempt: x-a1\n"),
        ),
        // An unfamiliar note word over plain prose: not an attempt either.
        message(
            "plain",
            "2026-10-26T21:01:00Z",
            "no front matter, no record",
        ),
        // A receipt without its `agent exit` line: not a parseable receipt.
        message(
            "short",
            "2026-10-26T21:02:00Z",
            "pool result j9: finished\n",
        ),
    ];
    let evidence = task_evidence(
        &thread,
        Window {
            offset: 0,
            limit: 10,
        },
        Window {
            offset: 0,
            limit: 10,
        },
    );
    assert_eq!(evidence.total, 3);
    assert!(
        evidence
            .messages
            .iter()
            .all(|row| !row.is_attempt && !row.body_truncated)
    );
    let rows = &evidence.messages;
    // Unstamped letters sort first by date; the stamped note follows.
    assert_eq!(rows[0].handle, "plain");
    assert_eq!(rows[1].handle, "short");
    assert_eq!(rows[2].handle, "bad");
    assert_eq!(rows[0].body, thread[1].body);
    assert_eq!(rows[1].body, thread[2].body);
    assert_eq!(rows[2].body, thread[0].body);
    assert!(evidence.attempts.is_empty());
    assert_eq!(evidence.attempts_total, 0);
    assert_eq!(evidence.attempts_next_offset, None);
    assert!(!evidence.attempts_truncated);
}

/// A record whose excerpt and final message are each far longer than the
/// tails an attempt row keeps; the excerpt ends on the 429 line.
fn long_record() -> String {
    let mut out = String::from(
        "pump: attempt record\nattempt: 52a1b306-a3\nneed: abf4884a\ntask: 52a1b306\n\
         pool: studio\noutcome: agent failed: hand failed\nfailing command:\n  agent exit 1\n\
         excerpt:\n",
    );
    for n in 0..80 {
        let _ = writeln!(out, "  build line {n}: compiling crate_{n}");
    }
    out.push_str("  429: {\"code\":\"1302\",\"message\":\"Rate limit reached\"}\n");
    out.push_str("hand's final message:\n");
    for n in 0..80 {
        let _ = writeln!(out, "  verbose prose line {n}: the hand saw the limit");
    }
    out
}

#[test]
fn attempt_body_preserves_failure_excerpt_tail() {
    let huge = "x".repeat(BODY_CAP + 1);
    let thread = [
        message("huge", "2026-10-26T21:00:00Z", &huge),
        message("fail", "2026-10-26T21:01:00Z", &long_record()),
    ];
    let evidence = task_evidence(
        &thread,
        Window {
            offset: 0,
            limit: 10,
        },
        Window {
            offset: 0,
            limit: 10,
        },
    );
    // Ordinary bodies cap from the head and say so.
    let huge_row = &evidence.messages[0];
    assert!(huge_row.body_truncated && huge_row.body.len() <= BODY_CAP);
    // The attempt row keeps the evidence: outcome and failing command first,
    // then the excerpt tail with the 429 line, and only then the prose tail.
    let row = &evidence.attempts[0];
    assert_eq!(row.handle, "fail");
    assert_eq!(row.fields.attempt, "52a1b306-a3");
    assert_eq!(row.fields.outcome, "agent failed: hand failed");
    assert!(
        row.body
            .starts_with("outcome: agent failed: hand failed\nfailing command: agent exit 1\n")
    );
    assert!(row.body.contains("excerpt tail:"));
    assert!(row.body.contains("429: {\"code\":\"1302\""));
    assert!(!row.body.contains("build line 0:"));
    assert!(row.body.contains("message tail:"));
    assert!(row.body.contains("verbose prose line 79"));
    assert!(!row.body.contains("verbose prose line 0:"));
    assert!(row.body_truncated);
}
