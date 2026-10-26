//! Tests for the flow retry classifier: table-driven positives and negatives
//! over the machine evidence a finished step job carries.

use super::{EMPTY_REPLY, PROVIDER, RUNNER, classify};
use crate::pump::jobs::{JobOutcome, JobSpec, JobStatus, PoolJob};

/// A pool receipt: the summary line, an optional stderr tail and the hand's
/// own final message.
fn receipt(exit: i32, stderr: &str, message: &str) -> String {
    let mut out = format!("agent exit {exit} in 9s\n");
    if !stderr.is_empty() {
        out.push_str(&format!("\n--- stderr tail\n{stderr}\n"));
    }
    out.push_str(&format!("\n--- receipt\n{message}\n"));
    out
}

fn job(status: JobStatus, state: &str, detail: &str, receipt_text: &str) -> PoolJob {
    model_job(None, status, state, detail, receipt_text)
}

/// A model job; `command` set turns it into a model-free command job.
fn model_job(
    command: Option<&str>,
    status: JobStatus,
    state: &str,
    detail: &str,
    receipt_text: &str,
) -> PoolJob {
    let mut job = PoolJob::from(JobSpec::new("step-x", "slice", "hand", "s", "b"));
    job.command = command.map(str::to_owned);
    job.status = status;
    job.outcome = Some(JobOutcome {
        state: state.to_owned(),
        detail: detail.to_owned(),
        receipt: receipt_text.to_owned(),
        at: 0,
    });
    job
}

/// Positive evidence: every row must classify, under the named class.
fn positive() -> Vec<(&'static str, String, String, &'static str)> {
    vec![
        (
            "connection error in stderr",
            String::new(),
            receipt(1, "Error: Connection error while streaming", "tried twice"),
            PROVIDER,
        ),
        (
            "connection refused in stderr",
            String::new(),
            receipt(1, "provider: connection refused", ""),
            PROVIDER,
        ),
        (
            "connection reset by peer",
            String::new(),
            receipt(1, "read: connection reset by peer", ""),
            PROVIDER,
        ),
        (
            "HTTP 429 in stderr",
            String::new(),
            receipt(1, "provider api returned HTTP 429", ""),
            PROVIDER,
        ),
        (
            "bare 429 token in detail",
            "step exit 1: 429 from upstream".into(),
            receipt(1, "", ""),
            PROVIDER,
        ),
        (
            "rate limit wording",
            String::new(),
            receipt(1, "rate limit exceeded for org", ""),
            PROVIDER,
        ),
        (
            "overloaded wording",
            String::new(),
            receipt(1, "upstream overloaded_error, try again", ""),
            PROVIDER,
        ),
        (
            "too many requests wording",
            String::new(),
            receipt(1, "429 Too Many Requests", ""),
            PROVIDER,
        ),
        (
            "gateway 502 line",
            String::new(),
            receipt(1, "gateway: HTTP 502 Bad Gateway", ""),
            PROVIDER,
        ),
        (
            "service unavailable line",
            String::new(),
            receipt(1, "503 Service Unavailable", ""),
            PROVIDER,
        ),
        (
            "gateway timeout line",
            String::new(),
            receipt(1, "the api answered 504 gateway timeout", ""),
            PROVIDER,
        ),
        (
            "runner fetch_failed detail",
            "fetch_failed exited 128".into(),
            receipt(0, "", ""),
            RUNNER,
        ),
        (
            "runner checkout_failed detail",
            "checkout_failed no such ref".into(),
            receipt(0, "", ""),
            RUNNER,
        ),
        (
            "empty reply after clean exit",
            String::new(),
            receipt(0, "", ""),
            EMPTY_REPLY,
        ),
        (
            "whitespace reply",
            String::new(),
            receipt(0, "", "   \n\t  "),
            EMPTY_REPLY,
        ),
        (
            "literal null reply",
            String::new(),
            receipt(0, "", "null"),
            EMPTY_REPLY,
        ),
    ]
}

/// Negative evidence: every row must stay unclassified.
fn negative() -> Vec<(&'static str, String, String)> {
    vec![
        (
            "ordinary test failure",
            String::new(),
            receipt(
                101,
                "test result: FAILED. 1 failed; 12 passed",
                "fixed the rest",
            ),
        ),
        (
            "auth rejection 401",
            String::new(),
            receipt(1, "provider: HTTP 401 Unauthorized", ""),
        ),
        (
            "auth rejection 403",
            String::new(),
            receipt(1, "api: 403 Forbidden", ""),
        ),
        (
            "invalid api key",
            String::new(),
            receipt(1, "provider: invalid_api_key for model m", ""),
        ),
        (
            "invalid credentials wording",
            String::new(),
            receipt(1, "login: invalid credentials, check config", ""),
        ),
        (
            "auth rejection wins over empty reply",
            String::new(),
            receipt(0, "provider: invalid api key", ""),
        ),
        (
            "missing tool wins over empty reply",
            String::new(),
            receipt(127, "bash: tsc: command not found", ""),
        ),
        (
            "nonzero exit wins over empty reply",
            String::new(),
            receipt(101, "test result: FAILED. 1 failed", ""),
        ),
        (
            "near-miss status numbers",
            String::new(),
            receipt(1, "assert_eq!(got, 1429); // and 4290 elsewhere", ""),
        ),
    ]
}

#[test]
fn positive_evidence_classifies() {
    for (name, detail, receipt_text, expected) in positive() {
        let job = job(JobStatus::Finished, "finished", &detail, &receipt_text);
        assert_eq!(classify(&job), Some(expected), "{name}");
    }
}

#[test]
fn negative_evidence_stays_unclassified() {
    for (name, detail, receipt_text) in negative() {
        let job = job(JobStatus::Finished, "finished", &detail, &receipt_text);
        assert_eq!(classify(&job), None, "{name}");
    }
}

#[test]
fn http_auth_distinction() {
    let limited = job(
        JobStatus::Finished,
        "finished",
        "",
        &receipt(1, "HTTP 429 from upstream", ""),
    );
    let rejected = job(
        JobStatus::Finished,
        "finished",
        "",
        &receipt(1, "HTTP 401 Unauthorized", ""),
    );
    let forbidden = job(
        JobStatus::Finished,
        "finished",
        "",
        &receipt(1, "HTTP 403 Forbidden", ""),
    );
    assert_eq!(classify(&limited), Some(PROVIDER));
    assert_eq!(classify(&rejected), None);
    assert_eq!(classify(&forbidden), None);
}

/// Provider words in the hand's own valid message are content, never
/// evidence: the scan scope stops before the message section.
#[test]
fn provider_words_in_valid_content_do_not_retry() {
    let message = "the provider returned 429 and a connection error during my run; \
                   a checkout failure was mentioned in the log, and the gateway said \
                   service unavailable once, but I finished: plan is ship it";
    let job = job(
        JobStatus::Finished,
        "finished",
        "",
        &receipt(0, "", message),
    );
    assert_eq!(classify(&job), None);
}

/// Model-free command jobs need no envelope: no prose requirement and no
/// provider reading of their ordinary output. Runner fetch/checkout outcomes
/// are job-kind-agnostic and stay retryable, so those rows are skipped.
#[test]
fn command_jobs_are_never_provider_or_empty_reply() {
    for (name, detail, receipt_text, expected) in positive() {
        if expected == RUNNER {
            continue;
        }
        let job = model_job(
            Some("cargo test"),
            JobStatus::Finished,
            "finished",
            &detail,
            &receipt_text,
        );
        assert_eq!(classify(&job), None, "{name}");
    }
    let failing = model_job(
        Some("cargo test"),
        JobStatus::Finished,
        "finished",
        "",
        &receipt(
            101,
            "test result: FAILED. 1 failed; output said 429 rate limit",
            "",
        ),
    );
    assert_eq!(classify(&failing), None);
}

#[test]
fn unfinished_jobs_are_not_read() {
    for status in [JobStatus::Pending, JobStatus::Dispatched, JobStatus::Lost] {
        let job = job(status, "finished", "429 connection error", "");
        assert_eq!(classify(&job), None, "{status:?}");
    }
    assert_eq!(classify(&job(JobStatus::Timeout, "finished", "", "")), None);
    for state in ["timeout", "lost"] {
        let job = job(JobStatus::Finished, state, "429 connection error", "");
        assert_eq!(classify(&job), None, "{state}");
    }
}

#[test]
fn a_finished_job_with_no_outcome_is_not_read() {
    let mut job = PoolJob::from(JobSpec::new("step-x", "slice", "hand", "s", "b"));
    job.status = JobStatus::Finished;
    assert_eq!(classify(&job), None);
}

/// The runner outcome is the detail's first word, as the pool writes it:
/// the same words anywhere else are ordinary content, not runner evidence.
#[test]
fn runner_outcome_requires_the_details_first_word() {
    let detail = job(
        JobStatus::Finished,
        "finished",
        "push rejected: fetch_failed",
        &receipt(0, "", "delivered the slice"),
    );
    let stderr = job(
        JobStatus::Finished,
        "finished",
        "",
        &receipt(1, "push outcome fetch_failed", ""),
    );
    assert_eq!(classify(&detail), None);
    assert_eq!(classify(&stderr), None);
}

#[test]
fn a_clean_exit_with_a_substantive_reply_is_not_transient() {
    let job = job(
        JobStatus::Finished,
        "finished",
        "",
        &receipt(0, "", "landed the slice, tests green"),
    );
    assert_eq!(classify(&job), None);
}

#[test]
fn null_reply_is_case_insensitive_and_padded() {
    for message in ["NULL", "Null", "  null  \n"] {
        let job = job(
            JobStatus::Finished,
            "finished",
            "",
            &receipt(0, "", message),
        );
        assert_eq!(classify(&job), Some(EMPTY_REPLY), "{message}");
    }
}
