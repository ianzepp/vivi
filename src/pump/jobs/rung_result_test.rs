use super::{TAIL_BYTES, TAIL_LINES, parse};
use crate::pump::jobs::{JobOutcome, JobSpec, JobStatus, PoolJob};

fn job(status: JobStatus, receipt: Option<&str>) -> PoolJob {
    let mut job = PoolJob::from(JobSpec::new("rung-1", "rung", "pool", "s", "b"));
    job.status = status;
    job.outcome = receipt.map(|receipt| JobOutcome {
        state: String::from("finished"),
        detail: String::new(),
        receipt: receipt.to_owned(),
        at: 1,
    });
    job
}

#[test]
fn exit_zero_passes() {
    let result = parse(&job(JobStatus::Finished, Some("out\nagent exit 0 in 8s")));
    assert!(result.pass);
    assert_eq!(result.exit, Some(0));
    assert!(result.tail.contains("out"));
}

#[test]
fn exit_one_fails_with_its_code() {
    let result = parse(&job(JobStatus::Finished, Some("boom\nagent exit 1 in 8s")));
    assert!(!result.pass);
    assert_eq!(result.exit, Some(1));
}

#[test]
fn timeout_and_lost_have_no_exit_even_with_a_zero_receipt() {
    for status in [JobStatus::Timeout, JobStatus::Lost] {
        let result = parse(&job(status, Some("agent exit 0 in 8s")));
        assert!(!result.pass);
        assert_eq!(result.exit, None);
    }
}

#[test]
fn a_missing_exit_or_receipt_does_not_pass() {
    let no_exit = parse(&job(JobStatus::Finished, Some("no summary")));
    assert!(!no_exit.pass);
    assert_eq!(no_exit.exit, None);
    let none = parse(&job(JobStatus::Finished, None));
    assert!(!none.pass);
    assert_eq!(none.exit, None);
    assert_eq!(none.tail, "");
}

#[test]
fn the_tail_keeps_the_last_lines() {
    let receipt: String = (0..200).map(|i| format!("line{i}\n")).collect();
    let result = parse(&job(JobStatus::Finished, Some(&receipt)));
    let lines: Vec<&str> = result.tail.lines().collect();
    assert_eq!(lines.len(), TAIL_LINES);
    assert_eq!(lines.last(), Some(&"line199"));
    assert_eq!(lines[0], "line140");
}

#[test]
fn the_tail_is_capped_in_bytes_on_a_char_boundary() {
    let receipt = "é".repeat(TAIL_BYTES);
    let result = parse(&job(JobStatus::Finished, Some(&receipt)));
    assert!(result.tail.len() <= TAIL_BYTES);
    assert!(result.tail.ends_with('é'));
}

fn posted(rc: &str, receipt: &str, stderr: &str, log: &str) -> String {
    use crate::pool::collect::{Collected, JobResult, Outcome};
    use crate::pool::record::{DispatchRecord, OpenDispatch, collected_text};
    use std::collections::BTreeMap;
    let record = DispatchRecord {
        target: None,
        repository: None,
        pool: "p".into(),
        need: None,
        kind: None,
        slot: "p-slot-1".into(),
        machine: "m1".into(),
        job: "rung-1".into(),
        model: "m/x".into(),
        from: "main".into(),
        branch: "factory/a".into(),
        start: "a".repeat(40),
        with: Vec::new(),
        budget: 600,
        at: 1000,
        read_only: false,
        hardware: String::new(),
        abandoned: 0,
        abandoned_cost_micros: 0,
    };
    let done = Collected {
        result: JobResult {
            fields: BTreeMap::from([("agent_rc".to_string(), rc.to_string())]),
        },
        receipt: receipt.into(),
        stderr: stderr.into(),
        log: log.into(),
    };
    let open = OpenDispatch {
        task: "aaaa1111".into(),
        record,
    };
    collected_text(&open, &Outcome::Finished(done))
}

#[test]
fn the_tail_is_the_receipt_end_then_stderr_never_the_runner_log() {
    let log: String = (0..200).map(|i| format!("runner-log-{i}\n")).collect();
    let text = posted(
        "101",
        "running 2 tests\ntest pump::flow::a ... FAILED\n",
        "error: test failed, to rerun pass `--lib`\n",
        &log,
    );
    let result = parse(&job(JobStatus::Finished, Some(&text)));
    assert!(!result.pass);
    assert_eq!(result.exit, Some(101));
    assert!(result.tail.contains("test pump::flow::a ... FAILED"));
    assert!(result.tail.contains("error: test failed"));
    assert!(!result.tail.contains("runner-log-"));
    assert!(result.tail.find("FAILED") < result.tail.find("error: test failed"));
}

#[test]
fn the_tail_omits_the_facts_and_a_reply_with_only_a_log_has_an_empty_tail() {
    let text = posted("0", "ok\n", "", "only-log-line\n");
    let result = parse(&job(JobStatus::Finished, Some(&text)));
    assert!(result.tail.contains("ok"));
    assert!(!result.tail.contains("only-log-line"));
    assert!(!result.tail.contains("pool result"));
    let only_log = posted("0", "", "", "only-log-line\n");
    let result = parse(&job(JobStatus::Finished, Some(&only_log)));
    assert_eq!(result.tail, "");
}

#[test]
fn a_long_receipt_section_is_still_capped_by_lines() {
    let receipt: String = (0..200).map(|i| format!("line{i}\n")).collect();
    let text = posted("1", &receipt, "", "");
    let result = parse(&job(JobStatus::Finished, Some(&text)));
    assert_eq!(result.tail.lines().count(), TAIL_LINES);
    assert_eq!(result.tail.lines().last(), Some("line199"));
}
