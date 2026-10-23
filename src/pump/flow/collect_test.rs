//! Tests for collecting role-step answers.

use super::{Collected, collect};
use crate::pump::flow::def::NodeClass;
use crate::pump::flow::index::StepJobRow;
use crate::pump::jobs::{JobOutcome, JobSpec, JobStatus, PoolJob};

fn job(status: JobStatus, receipt: &str) -> PoolJob {
    let mut job = PoolJob::from(JobSpec::new("j1", "researcher", "researcher", "s", "b"));
    job.status = status;
    job.outcome = Some(JobOutcome {
        state: "finished".to_owned(),
        detail: String::new(),
        receipt: receipt.to_owned(),
        at: 0,
    });
    job
}

fn step(attempt: u32) -> StepJobRow {
    StepJobRow {
        job: "j1".to_owned(),
        run: 1,
        node: "research".to_owned(),
        task: "t1".to_owned(),
        attempt,
    }
}

fn finished(message: &str) -> PoolJob {
    job(JobStatus::Finished, message)
}

#[test]
fn a_valid_word_is_the_outcome_with_the_message_as_payload() {
    let message = "Findings above.\nANSWER: READY";
    let got = collect(&finished(message), &step(1), &NodeClass::Researcher);
    assert_eq!(
        got,
        Collected::Outcome("READY".to_owned(), serde_json::json!(message))
    );
}

#[test]
fn the_last_answer_line_wins() {
    let message = "ANSWER: NOT READY\nrethought\nANSWER: READY";
    let got = collect(&finished(message), &step(1), &NodeClass::Researcher);
    assert!(matches!(got, Collected::Outcome(word, _) if word == "READY"));
}

#[test]
fn not_ready_matches_as_a_whole() {
    let got = collect(
        &finished("ANSWER: NOT READY"),
        &step(1),
        &NodeClass::Researcher,
    );
    assert!(matches!(got, Collected::Outcome(word, _) if word == "NOT READY"));
}

#[test]
fn a_word_outside_the_class_set_is_no_answer() {
    let got = collect(
        &finished("ANSWER: merged"),
        &step(1),
        &NodeClass::Researcher,
    );
    assert_eq!(got, Collected::Retry);
}

#[test]
fn a_missing_answer_line_retries_once() {
    let got = collect(&finished("all done"), &step(1), &NodeClass::Planner);
    assert_eq!(got, Collected::Retry);
}

#[test]
fn the_second_failure_gives_failed() {
    let got = collect(&finished("all done"), &step(2), &NodeClass::Planner);
    assert_eq!(got, Collected::Failed);
}

#[test]
fn timeout_and_lost_retry_then_fail() {
    for status in [JobStatus::Timeout, JobStatus::Lost] {
        let first = collect(
            &job(status, "ANSWER: READY"),
            &step(1),
            &NodeClass::Researcher,
        );
        assert_eq!(first, Collected::Retry);
        let second = collect(
            &job(status, "ANSWER: READY"),
            &step(2),
            &NodeClass::Researcher,
        );
        assert_eq!(second, Collected::Failed);
    }
}

#[test]
fn a_hand_that_answers_failed_is_an_outcome() {
    let got = collect(&finished("ANSWER: failed"), &step(1), &NodeClass::Judge);
    assert!(matches!(got, Collected::Outcome(word, _) if word == "failed"));
}
