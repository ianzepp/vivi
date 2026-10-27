//! Tests for the shared transient-recovery helpers.

use super::{
    EMPTY, PAYLOAD_KEY, PROVIDER, RUNNER, RecoveryMeta, base_id, metadata, next_replacement_id,
    retries_spent, stamp_metadata, transient_class, transient_retry_n,
};
use crate::pump::envelope::correct;
use crate::pump::jobs::{JobOutcome, JobSpec, JobStatus, PoolJob};

/// A durable job in `status` with an empty payload and no outcome.
fn job(status: JobStatus) -> PoolJob {
    let mut job = PoolJob::from(JobSpec::new("j", "step", "hand", "subject", "body"));
    job.status = status;
    job
}

/// A finished job whose reconcile detail and collected receipt are given.
fn finished(detail: &str, receipt: &str) -> PoolJob {
    let mut job = job(JobStatus::Finished);
    job.outcome = Some(JobOutcome {
        state: "finished".to_owned(),
        detail: detail.to_owned(),
        receipt: receipt.to_owned(),
        at: 7,
    });
    job
}

/// A receipt as the pool writes it: summary lines, then sections.
fn receipt(exit: i32, message: &str, stderr: &str) -> String {
    receipt_log(exit, message, stderr, "log line")
}

/// A receipt whose runner-log tail is `log`.
fn receipt_log(exit: i32, message: &str, stderr: &str, log: &str) -> String {
    format!(
        "pool result j1: finished\nagent exit {exit} in 3s, job 9s, started from 1234567890\n\n\
         --- receipt\n{message}\n\n--- stderr tail\n{stderr}\n\n--- runner log tail\n{log}"
    )
}

/// A finished model job with a clean exit and no machine trouble at all.
fn clean() -> PoolJob {
    finished(
        "finished push=committed",
        &receipt(0, "answer = \"built\"", ""),
    )
}

#[test]
fn transient_classifier_ignores_model_prose_and_real_failures() {
    // The hand's own message section never counts, however loudly it names
    // infrastructure trouble: prose about a 429 or a dropped connection is
    // a work report, not machine evidence.
    let prose = receipt(
        1,
        "the API returned 429 and the connection was refused; the upstream \
         was overloaded, so I could not finish the unit",
        "",
    );
    assert_eq!(transient_class(&finished("", &prose)), None);
    // A genuine work failure: real failure output, no machine marker.
    let work = receipt(
        1,
        "answer = \"failed\"\n\nthe check fails on main",
        "panicked at src/lib.rs: test left == right",
    );
    assert_eq!(transient_class(&finished("", &work)), None);
    // A missing tool is a host failure, never a transient retry.
    let tool = receipt(127, "", "zsh:1: command not found: pi");
    assert_eq!(transient_class(&finished("", &tool)), None);
    // Authentication trouble is not uniformly retryable: a rejected key or
    // an unauthorized response names configuration, not a blip.
    let auth = receipt(1, "", "error: 401 Unauthorized\ninvalid_api_key: sk-bad");
    assert_eq!(transient_class(&finished("", &auth)), None);
    // A malformed nonempty envelope is a correction case.
    let malformed = receipt(0, "not an envelope at all", "");
    assert_eq!(transient_class(&finished("", &malformed)), None);
    // A clean success is nothing to recover.
    assert_eq!(transient_class(&clean()), None);
}

#[test]
fn machine_evidence_classes() {
    // Runner fetch and checkout failures come from the reconcile detail.
    assert_eq!(
        transient_class(&finished(
            "checkout_failed no such ref",
            &receipt(1, "", "")
        )),
        Some(RUNNER)
    );
    assert_eq!(
        transient_class(&finished(
            "fetch_failed could not read remote",
            &receipt(1, "", "")
        )),
        Some(RUNNER)
    );
    // Credential outcomes are host configuration, not transient.
    assert_eq!(
        transient_class(&finished(
            "target_key_invalid bad token",
            &receipt(1, "", "")
        )),
        None
    );
    // A 429 in the stderr tail.
    let rate = receipt(1, "my answer", "429: rate limit reached for requests");
    assert_eq!(transient_class(&finished("", &rate)), Some(PROVIDER));
    // Temporary upstream words match lowercased.
    let upstream = receipt(1, "my answer", "error sending request: 502 Bad Gateway");
    assert_eq!(transient_class(&finished("", &upstream)), Some(PROVIDER));
    let unavailable = receipt(1, "my answer", "503 Service Unavailable");
    assert_eq!(transient_class(&finished("", &unavailable)), Some(PROVIDER));
    let saturated = receipt(1, "my answer", "upstream_error: overloaded_error");
    assert_eq!(transient_class(&finished("", &saturated)), Some(PROVIDER));
    // A dropped transport in the runner log tail.
    let dropped = receipt_log(
        1,
        "my answer",
        "",
        "dispatch: connection refused by upstream",
    );
    assert_eq!(transient_class(&finished("", &dropped)), Some(PROVIDER));
    // The bounded summary head of a legacy receipt still counts: no
    // sections, so the pool summary is machine scope up to the boundary.
    let legacy = "pool result j1: finished\nagent exit 1 in 3s\n\
                  Connection error while sending request\n\n\
                  hand's final message:\nthe upstream was overloaded, I retried\n";
    assert_eq!(transient_class(&finished("", legacy)), Some(PROVIDER));
    // After that boundary the hand's prose never counts.
    let legacy_prose = "pool result j1: finished\nagent exit 1 in 3s\n\n\
                        hand's final message:\nI saw a 429 and gave up\n";
    assert_eq!(transient_class(&finished("", legacy_prose)), None);
}

#[test]
fn empty_and_null_final_messages_are_transient() {
    // A finished model job whose final message is blank.
    assert_eq!(
        transient_class(&finished("", &receipt(0, "", ""))),
        Some(EMPTY)
    );
    assert_eq!(
        transient_class(&finished("", &receipt(0, "   \n\t", ""))),
        Some(EMPTY)
    );
    // The adapter null-result representation: the JSON `null` literal.
    assert_eq!(
        transient_class(&finished("", &receipt(0, "null", ""))),
        Some(EMPTY)
    );
    assert_eq!(
        transient_class(&finished("", &receipt(0, "  null\n", ""))),
        Some(EMPTY)
    );
    // A real message is never a null result, whatever it says.
    assert_eq!(
        transient_class(&finished("", &receipt(0, "nullish", ""))),
        None
    );
    assert_eq!(
        transient_class(&finished("", &receipt(0, "NULL", ""))),
        None
    );
    // A blank reply at a nonzero exit is a dead harness — a missing tool, a
    // crashed adapter — never a genuinely empty answer.
    assert_eq!(transient_class(&finished("", &receipt(1, "", ""))), None);
    assert_eq!(transient_class(&finished("", &receipt(127, "", ""))), None);
}

#[test]
fn command_jobs_never_classify() {
    // A command job has no model final message: empty stdout on success is
    // ordinary, and its failures are genuine work evidence.
    let mut success = finished("", &receipt(0, "", ""));
    success.command = Some("make test".to_owned());
    assert_eq!(transient_class(&success), None);
    let mut failed = finished("", &receipt(1, "", "curl: connection refused"));
    failed.command = Some("curl example.com".to_owned());
    assert_eq!(transient_class(&failed), None);
}

#[test]
fn budget_kills_are_excluded_in_both_representations() {
    // Representation one: the pool's terminal timeout status.
    let mut timeout = job(JobStatus::Timeout);
    timeout.outcome = Some(JobOutcome {
        state: "timeout".to_owned(),
        detail: String::new(),
        receipt: String::new(),
        at: 7,
    });
    assert_eq!(transient_class(&timeout), None);
    // Representation two: a finished summary reporting the timeout exit.
    let killed = finished(
        "",
        "pool result j1: finished\nagent exit 124 after 600s\n\n--- receipt\ncut off",
    );
    assert_eq!(transient_class(&killed), None);
    // A lost job is the recovery machinery's business, not a retry class.
    let mut lost = job(JobStatus::Lost);
    lost.outcome = Some(JobOutcome {
        state: "lost".to_owned(),
        detail: String::new(),
        receipt: receipt(1, "", "connection refused"),
        at: 7,
    });
    assert_eq!(transient_class(&lost), None);
}

#[test]
fn replacement_identity_is_deterministic_and_stacking_free() {
    assert_eq!(base_id("j"), "j");
    assert_eq!(transient_retry_n("j"), 0);
    assert_eq!(base_id("j-tr1"), "j");
    assert_eq!(transient_retry_n("j-tr12"), 12);
    assert_eq!(next_replacement_id("j"), "j-tr1");
    // Re-scheduling a failed retry advances the ordinal; suffixes never
    // stack.
    assert_eq!(next_replacement_id("j-tr1"), "j-tr2");
    assert_eq!(next_replacement_id("j-tr3"), "j-tr4");
    // A correction of a retry names the same step input.
    assert_eq!(base_id("j-tr1-c1"), "j");
    assert_eq!(transient_retry_n("j-tr1-c1"), 1);
    assert_eq!(next_replacement_id("j-tr1-c1"), "j-tr2");
    // A correction alone is not a transient retry; the base strips it the
    // same way it strips a retry ordinal, so the failed correction of an
    // unretried input advances that input's first ordinal.
    assert_eq!(base_id("j-c1"), "j");
    assert_eq!(transient_retry_n("j-c1"), 0);
    assert_eq!(next_replacement_id("j-c1"), "j-tr1");
}

#[test]
fn correction_parsing_recognizes_the_transient_suffix() {
    // A transient retry id is an original job to the correction loop.
    assert_eq!(correct::base_id("j-tr1"), "j-tr1");
    assert_eq!(correct::correction_n("j-tr1"), 0);
    // And a correction of a retry chains off the retry, so both vocabularies
    // can name the same job without ambiguity.
    assert_eq!(correct::base_id("j-tr1-c1"), "j-tr1");
    assert_eq!(correct::correction_n("j-tr1-c1"), 1);
}

#[test]
fn metadata_round_trips_and_defaults_to_the_initial_input() {
    let meta = |retry: u32| RecoveryMeta {
        input: "task-9".to_owned(),
        retry,
        class: PROVIDER.to_owned(),
    };
    let mut payload = serde_json::json!({});
    // Absent metadata is the initial input: nothing spent, and never a
    // fresh autonomous allowance.
    assert_eq!(metadata(&payload), None);
    assert_eq!(retries_spent(&payload), 0);
    stamp_metadata(&mut payload, &meta(1));
    assert_eq!(metadata(&payload), Some(meta(1)));
    assert_eq!(retries_spent(&payload), 1);
    // The step input is identified independently of ordinary attempts and
    // correction ordinals: the reader never consults either.
    payload["attempt"] = serde_json::Value::from(5);
    assert_eq!(metadata(&payload), Some(meta(1)));
    // Re-stamping advances the ordinal the reader reports.
    stamp_metadata(&mut payload, &meta(2));
    assert_eq!(retries_spent(&payload), 2);
    // Other payload keys survive.
    assert_eq!(payload.get("attempt"), Some(&serde_json::Value::from(5)));
}

#[test]
fn metadata_stamping_respects_payload_shapes() {
    let meta = RecoveryMeta {
        input: "t".to_owned(),
        retry: 2,
        class: EMPTY.to_owned(),
    };
    // A null payload grows the object.
    let mut null = serde_json::Value::Null;
    stamp_metadata(&mut null, &meta);
    assert_eq!(retries_spent(&null), 2);
    // Another shape is left untouched.
    let mut scalar = serde_json::Value::from(7);
    stamp_metadata(&mut scalar, &meta);
    assert_eq!(scalar, serde_json::Value::from(7));
    // Unreadable metadata is the initial input: no ordinal, no allowance.
    let broken = serde_json::json!({ PAYLOAD_KEY: { "input": 3 } });
    assert_eq!(metadata(&broken), None);
    assert_eq!(retries_spent(&broken), 0);
    let partial = serde_json::json!({ PAYLOAD_KEY: { "input": "t", "retry": 1 } });
    assert_eq!(metadata(&partial), None);
    assert_eq!(retries_spent(&partial), 0);
}
