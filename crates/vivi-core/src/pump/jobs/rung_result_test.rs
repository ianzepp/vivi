use super::{EnvFailure, EnvKind, TAIL_BYTES, TAIL_LINES, env_failure, parse};
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
    posted_facts(rc, Vec::new(), receipt, stderr, log)
}

/// A posted reply whose facts section carries one `$ <cmd>` fact with
/// `<out>` as its output.
fn posted_fact(rc: &str, cmd: &str, out: &str, receipt: &str, stderr: &str, log: &str) -> String {
    posted_facts(rc, vec![(cmd, out)], receipt, stderr, log)
}

fn posted_facts(
    rc: &str,
    facts: Vec<(&str, &str)>,
    receipt: &str,
    stderr: &str,
    log: &str,
) -> String {
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
    let mut fields = BTreeMap::from([("agent_rc".to_string(), rc.to_string())]);
    for (n, (cmd, out)) in facts.iter().enumerate() {
        fields.insert(format!("fact_{}_cmd", n + 1), (*cmd).to_string());
        fields.insert(format!("fact_{}_exit", n + 1), "0".to_string());
        fields.insert(format!("fact_{}_out", n + 1), (*out).to_string());
    }
    let done = Collected {
        result: JobResult { fields },
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

#[test]
fn an_unsectioned_reply_is_still_read_whole() {
    let receipt = format!("{}test a::one ... FAILED\n", running("alpha"));
    let result = parse(&job(JobStatus::Finished, Some(&receipt)));
    let ids: Vec<String> = result.failing.into_iter().collect();
    assert_eq!(ids, ["alpha::a::one"]);
    assert!(result.tail.contains("test a::one ... FAILED"));
}

#[test]
fn selection_is_receipt_then_stderr_and_survives_an_unknown_heading() {
    let receipt = "alpha-receipt-line\n\n--- document note\nbeta-receipt-after-unknown-heading\n";
    let text = posted("1", receipt, "gamma-stderr-line\n", "delta-log-line\n");
    let result = parse(&job(JobStatus::Finished, Some(&text)));
    assert!(result.tail.contains("alpha-receipt-line"));
    assert!(result.tail.contains("beta-receipt-after-unknown-heading"));
    assert!(result.tail.contains("gamma-stderr-line"));
    assert!(!result.tail.contains("delta-log-line"));
    assert!(!result.tail.contains("pool result"));
    assert!(result.tail.find("alpha").unwrap() < result.tail.find("gamma").unwrap());
}

fn failing_of(receipt: &str, stderr: &str) -> Vec<String> {
    let text = posted("101", receipt, stderr, "");
    let result = parse(&job(JobStatus::Finished, Some(&text)));
    result.failing.into_iter().collect()
}

fn running(binary: &str) -> String {
    format!("     Running unittests src/lib.rs (target/debug/deps/{binary}-1a2b3c4d5e6f7081)\n")
}

#[test]
fn binary_spellings_give_the_same_failing_set() {
    let hyphen = format!("{}test emit::a ... FAILED\n", running("mir-emit-harness"));
    let under = format!("{}test emit::a ... FAILED\n", running("mir_emit_harness"));
    let one = failing_of(&hyphen, "");
    assert_eq!(one, failing_of(&under, ""));
    assert_eq!(one, ["mir_emit_harness::emit::a"]);
}

#[test]
fn a_failure_is_prefixed_with_its_canonical_binary() {
    let receipt = "     Running unittests src/lib.rs (target/debug/deps/faber_cli-1a2b3c4d)\n\
                   test pump::x::y ... FAILED\n";
    assert_eq!(failing_of(receipt, ""), ["faber_cli::pump::x::y"]);
}

#[test]
fn two_binaries_with_one_failure_each_give_two_ids() {
    let receipt = format!(
        "{}test a::one ... FAILED\n{}test b::two ... FAILED\n",
        running("alpha"),
        running("beta-bin"),
    );
    assert_eq!(
        failing_of(&receipt, ""),
        ["alpha::a::one", "beta_bin::b::two"]
    );
}

#[test]
fn the_failures_block_dedups_with_the_failed_lines() {
    let receipt = format!(
        "{}test a::one ... FAILED\n\nfailures:\n\n---- a::one stdout ----\n    noise\n\nfailures:\n    a::one\n\ntest result: FAILED.\n",
        running("alpha"),
    );
    assert_eq!(failing_of(&receipt, ""), ["alpha::a::one"]);
}

#[test]
fn a_rerun_footer_names_the_binary_when_one_failed() {
    let one = failing_of(
        "test a::one ... FAILED\n",
        "error: test failed, to rerun pass `-p faber-cli --test cli_flow`\n",
    );
    assert_eq!(one, ["cli_flow::a::one"]);
    let two = failing_of(
        "test a::one ... FAILED\n",
        "to rerun pass `--lib`\nto rerun pass `--test x`\n",
    );
    assert_eq!(two, ["a::one"]);
}

#[test]
fn a_passing_run_and_a_compile_error_have_an_empty_set() {
    let pass = posted("0", "test a ... ok\ntest result: ok.\n", "", "");
    let result = parse(&job(JobStatus::Finished, Some(&pass)));
    assert!(result.pass && result.failing.is_empty());
    let build = posted("101", "", "error[E0432]: unresolved import\n", "");
    let result = parse(&job(JobStatus::Finished, Some(&build)));
    assert!(!result.pass && result.failing.is_empty());
}

#[test]
fn a_json_failure_line_is_parsed() {
    let receipt = format!(
        "{}{{\"type\":\"test\",\"event\":\"failed\",\"name\":\"m::t\"}}\n{{\"type\":\"test\",\"event\":\"ok\",\"name\":\"m::u\"}}\n",
        running("alpha"),
    );
    assert_eq!(failing_of(&receipt, ""), ["alpha::m::t"]);
}

#[test]
fn failing_ids_survive_unknown_and_backward_headings_in_the_receipt() {
    let receipt = format!(
        "{}test a::one ... FAILED\n\n--- document note\ntest b::two ... FAILED\n\n--- receipt\ntest c::three ... FAILED\n",
        running("alpha"),
    );
    let stderr = "to rerun pass `--test stderrbin`\ntest d::four ... FAILED\n";
    assert_eq!(
        failing_of(&receipt, &stderr),
        [
            "alpha::a::one",
            "alpha::b::two",
            "alpha::c::three",
            "alpha::d::four"
        ],
    );
}

#[test]
fn facts_and_runner_log_never_reach_the_failing_set_or_tail() {
    let text = posted_fact(
        "101",
        "cargo nextest run",
        "error: test failed, to rerun pass `--test phony`",
        "test a::one ... FAILED\n",
        "",
        "",
    );
    let result = parse(&job(JobStatus::Finished, Some(&text)));
    let ids: Vec<String> = result.failing.into_iter().collect();
    assert_eq!(ids, ["a::one"]);
    assert!(!result.tail.contains("phony"));
    assert!(!result.tail.contains("cargo nextest"));

    let log = "     Running unittests src/lib.rs (target/debug/deps/logbin-1a2b3c4d5e6f7081)\n\
               test l::t ... FAILED\n\nfailures:\n    l::t\n";
    let only_log = posted("101", "", "", log);
    let result = parse(&job(JobStatus::Finished, Some(&only_log)));
    assert!(result.failing.is_empty());
    assert_eq!(result.tail, "");
}

fn with_detail(text: &str, detail: &str) -> PoolJob {
    let mut job = job(JobStatus::Finished, Some(text));
    if let Some(outcome) = job.outcome.as_mut() {
        outcome.detail = detail.to_owned();
    }
    job
}

fn env_of(rc: &str, receipt: &str, stderr: &str) -> Option<EnvFailure> {
    let text = posted(rc, receipt, stderr, "");
    env_failure(&job(JobStatus::Finished, Some(&text)))
}

fn env_of_log(rc: &str, receipt: &str, stderr: &str, log: &str) -> Option<EnvFailure> {
    env_failure(&job(
        JobStatus::Finished,
        Some(&posted(rc, receipt, stderr, log)),
    ))
}

#[test]
fn agent_exit_127_is_a_missing_tool() {
    let found = env_of("127", "", "").expect("env failure");
    assert_eq!(found.kind, EnvKind::ToolMissing);
    assert_eq!(found.tool, None);
    let found = env_of("126", "", "").expect("env failure");
    assert_eq!(found.kind, EnvKind::ToolMissing);
}

#[test]
fn a_bash_command_not_found_in_stderr_names_the_tool() {
    let found = env_of("1", "", "bash: tsc: command not found\n").expect("env failure");
    assert_eq!(found.kind, EnvKind::ToolMissing);
    assert_eq!(found.tool.as_deref(), Some("tsc"));
    assert!(found.evidence.contains("command not found"));
}

#[test]
fn the_dash_form_names_the_tool() {
    let found = env_of("2", "", "sh: 1: tsc: not found\n").expect("env failure");
    assert_eq!(found.kind, EnvKind::ToolMissing);
    assert_eq!(found.tool.as_deref(), Some("tsc"));
}

#[test]
fn exit_127_takes_the_tool_from_the_stderr_line() {
    let found = env_of("127", "", "bash: tool-x: command not found\n").expect("env failure");
    assert_eq!(found.kind, EnvKind::ToolMissing);
    assert_eq!(found.tool.as_deref(), Some("tool-x"));
}

#[test]
fn a_failed_checkout_is_a_host_setup_failure() {
    let text = posted("1", "", "", "");
    let found = env_failure(&with_detail(&text, "checkout_failed exited 128"));
    let found = found.expect("env failure");
    assert_eq!(found.kind, EnvKind::HostSetup);
    assert_eq!(found.tool, None);
}

#[test]
fn an_index_lock_is_a_host_setup_failure() {
    let stderr = "fatal: Unable to create '.x/index.lock': File exists.\n";
    let found = env_of("1", "", stderr).expect("env failure");
    assert_eq!(found.kind, EnvKind::HostSetup);
}

#[test]
fn an_ordinary_red_test_run_is_not_an_environment_failure() {
    let receipt = format!("{}test a::one ... FAILED\n", running("alpha"));
    let stderr = "error: test failed, to rerun pass `--lib`\nerror: file not found in fixture\n";
    assert_eq!(env_of("101", &receipt, stderr), None);
}

#[test]
fn a_passing_job_is_not_an_environment_failure() {
    assert_eq!(env_of("0", "ok\n", ""), None);
    let ok = job(JobStatus::Finished, Some("out\nagent exit 0 in 8s"));
    assert_eq!(env_failure(&ok), None);
    let bare = PoolJob::from(JobSpec::new("rung-1", "rung", "pool", "s", "b"));
    assert_eq!(env_failure(&bare), None);
}

#[test]
fn command_not_found_in_the_hand_message_or_receipt_is_ignored() {
    let text = posted("1", "bash: tsc: command not found\n", "", "");
    assert_eq!(env_failure(&job(JobStatus::Finished, Some(&text))), None);
    let message = format!("{text}\n\n--- hand final message\nsh: 1: tsc: not found\n");
    assert_eq!(env_failure(&job(JobStatus::Finished, Some(&message))), None);
}

#[test]
fn missing_tool_evidence_is_read_from_the_stderr_section_only() {
    let found = env_of(
        "1",
        "bash: ghost: command not found\n",
        "bash: real: command not found\n",
    )
    .expect("env failure");
    assert_eq!(found.tool.as_deref(), Some("real"));
    assert!(found.evidence.contains("real"));
    assert!(!found.evidence.contains("ghost"));
}

#[test]
fn missing_tool_evidence_after_an_unknown_stderr_heading_is_kept() {
    let stderr = "worker cleanup\n\n--- teardown note\nbash: terraform: command not found\n";
    let found = env_of("1", "", stderr).expect("env failure");
    assert_eq!(found.kind, EnvKind::ToolMissing);
    assert_eq!(found.tool.as_deref(), Some("terraform"));
    assert!(found.evidence.contains("terraform"));
}

#[test]
fn command_not_found_in_the_runner_log_is_not_evidence() {
    let found = env_of_log("127", "", "", "bash: tsc: command not found\n").expect("env failure");
    assert_eq!(found.kind, EnvKind::ToolMissing);
    assert_eq!(found.tool, None);
    assert_eq!(found.evidence, "agent exit 127");
}

#[test]
fn the_evidence_is_one_line_of_at_most_200_chars() {
    let long = format!("bash: tsc: command not found {}\nsecond\n", "x".repeat(400));
    let found = env_of("1", "", &long).expect("env failure");
    assert!(found.evidence.chars().count() <= 200);
    assert!(!found.evidence.contains('\n'));
}
