//! Tests for [`crate::pump::attempt_read`]: parsing attempt records and pool
//! result replies, and the close-line timing summary.

use crate::pump::attempt_read::{
    AttemptFields, human_secs, opened_epoch, parse_receipt, parse_record, timing_summary,
};

/// A full attempt record body: the block unit eb093a9c-3 writes, then its
/// prose tail (which holds a second `task:` line that must be ignored).
const RECORD: &str = "\
pump: attempt record
attempt: eb093a9c-3-a2
need: 8f2c1d40
task: c366c7b7
actor: mind@local
role: hand
pool: here
slot: s1
model: zai/glm-5.3
start: 999940
end: 1000000
agent-seconds: 12
outcome: delivered
unit: eb093a9c-3  attempt: 2  model: zai/glm-5.3  pool: here  target: default
branch: unit/eb093a9c-3-a2 (pushed to the target remote, kept)
task: 77cb29c4
base: origin/next
";

/// A pool result reply as `collected_text` writes it.
const RECEIPT: &str = "\
pool result j123: finished
pool fly4  slot s1  model zai/glm
from next  default
push: pushed unit/n1-1-a1 abcdef0123
agent exit 0 in 12s, job 20s, started from 1700000000";

/// One attempt of the timing summary, with an id and agent seconds.
fn attempt(id: &str, secs: Option<u64>) -> AttemptFields {
    AttemptFields {
        attempt: id.to_owned(),
        agent_seconds: secs,
        ..AttemptFields::default()
    }
}

#[test]
fn the_block_of_an_attempt_record_parses_field_by_field() {
    let fields = parse_record(RECORD).expect("record parses");
    assert_eq!(fields.attempt, "eb093a9c-3-a2");
    assert_eq!(fields.need, Some(String::from("8f2c1d40")));
    assert_eq!(fields.task, Some(String::from("c366c7b7")));
    assert_eq!(fields.actor, Some(String::from("mind@local")));
    assert_eq!(fields.role, Some(String::from("hand")));
    assert_eq!(fields.pool, Some(String::from("here")));
    assert_eq!(fields.slot, Some(String::from("s1")));
    assert_eq!(fields.model, Some(String::from("zai/glm-5.3")));
    assert_eq!(fields.start, Some(999_940));
    assert_eq!(fields.end, Some(1_000_000));
    assert_eq!(fields.agent_seconds, Some(12));
    assert_eq!(fields.outcome, "delivered");
}

#[test]
fn the_prose_task_line_after_the_block_is_ignored() {
    let body = RECORD.replace("task: c366c7b7\n", "");
    let fields = parse_record(&body).expect("record parses");
    assert_eq!(fields.task, None, "the prose `task:` line must not be read");
}

#[test]
fn absent_values_parse_as_none() {
    let body = RECORD
        .replace("task: c366c7b7", "task: none")
        .replace("slot: s1", "slot: none")
        .replace("start: 999940", "start: none");
    let fields = parse_record(&body).expect("record parses");
    assert_eq!(fields.task, None);
    assert_eq!(fields.slot, None);
    assert_eq!(fields.start, None);
    assert_eq!(fields.model, Some(String::from("zai/glm-5.3")));
}

#[test]
fn a_body_without_the_header_is_not_a_record() {
    let body = RECORD.replace("pump: attempt record", "pump: attempt records");
    assert_eq!(parse_record(&body), None);
    assert_eq!(parse_record("unit: eb093a9c-3\nattempt: x\n"), None);
}

#[test]
fn a_block_without_an_attempt_is_not_a_record() {
    let body = RECORD.replace("attempt: eb093a9c-3-a2\n", "");
    assert_eq!(parse_record(&body), None);
    let body = RECORD.replace("outcome: delivered\n", "");
    assert_eq!(parse_record(&body), None);
}

#[test]
fn a_pool_receipt_parses_to_the_same_fields() {
    let fields = parse_receipt(RECEIPT).expect("receipt parses");
    assert_eq!(fields.attempt, "j123");
    assert_eq!(fields.outcome, "finished");
    assert_eq!(fields.pool, Some(String::from("fly4")));
    assert_eq!(fields.slot, Some(String::from("s1")));
    assert_eq!(fields.model, Some(String::from("zai/glm")));
    assert_eq!(fields.start, Some(1_700_000_000));
    assert_eq!(fields.agent_seconds, Some(12));
}

#[test]
fn a_pool_receipt_reads_the_after_form_and_needs_both_markers() {
    let body = RECEIPT.replace(" in 12s", " after 600s");
    assert_eq!(parse_receipt(&body).map(|f| f.agent_seconds), Some(Some(600)));
    assert_eq!(parse_receipt("pool result j1: finished\n"), None);
    assert_eq!(parse_receipt("agent exit 0 in 12s\n"), None);
}

#[test]
fn human_secs_keeps_the_two_biggest_units() {
    assert_eq!(human_secs(3725), "1h2m");
    assert_eq!(human_secs(93_720), "1d2h");
    assert_eq!(human_secs(12), "12s");
    assert_eq!(human_secs(0), "0s");
    assert_eq!(human_secs(90_000), "1d1h");
}

#[test]
fn an_rfc3339_date_is_epoch_seconds() {
    assert_eq!(opened_epoch("2023-11-14T22:13:20Z"), Some(1_700_000_000));
    assert_eq!(opened_epoch("2026-10-19T04:03:09+00:00"), Some(1_792_382_589));
    assert_eq!(opened_epoch("not a date"), None);
}

#[test]
fn the_timing_summary_names_the_slowest_attempt() {
    let attempts = [attempt("n1-1-a1", Some(60)), attempt("n1-1-a2", Some(3600))];
    assert_eq!(
        timing_summary(Some(910_000), &attempts, 1_000_000),
        "timing: open-to-done 1d1h; attempts 2; agent time 1h1m; slowest n1-1-a2 (1h)"
    );
}

#[test]
fn the_timing_summary_says_unknown_and_none_when_it_must() {
    let attempts = [attempt("n1-1-a1", None), attempt("n1-1-a2", None)];
    assert_eq!(
        timing_summary(None, &attempts, 1_000_000),
        "timing: open-to-done unknown; attempts 2; agent time 0s; slowest none"
    );
    assert_eq!(
        timing_summary(None, &[], 1_000_000),
        "timing: open-to-done unknown; attempts 0; agent time 0s; slowest none"
    );
}
