//! Tests for the host-failure classifier, table-driven over the incident's
//! real dispatch evidence.

use super::attempt_record::sections_tail;
use super::envfail::{
    HOST_FAIL_WINDOW, HOST_PAUSE_AFTER, HOST_PUSH_OUTCOMES, classify, missing_tool,
};

/// A task receipt as the pool writes it: summary lines, then sections.
fn receipt(exit: i32, message: &str, stderr: &str) -> String {
    format!(
        "pool result j1: finished\npool fly4  slot s1  model zai/glm\nfrom next  default\n\
         push: pushed unit/u-1-a1 abcdef0123\nagent exit {exit} in 0s, job 12s, started from 1234567890\n\n\
         --- receipt\n{message}\n\n--- stderr tail\n{stderr}\n\n--- runner log tail\nlog line"
    )
}

/// One classification case: dispatch evidence and what it should yield.
struct Case {
    name: &'static str,
    why: &'static str,
    detail: &'static str,
    receipt: String,
    want: Option<(&'static str, Option<&'static str>)>,
}

/// A receipt whose facts section pins the missing tool, while the agent's
/// own summary exit stays clean — the evidence behind rule 3.
fn facts_receipt() -> String {
    format!(
        "pool result j1: failed\nagent exit 3 in 2s, job 9s, started from 1234567890\n\n\
         --- facts\n$ pi --version  (exit 127)\n  zsh:1: no such file or directory: pi\n\n\
         --- receipt\nthe hand reported a plain unit failure\n"
    )
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "agent exit 127 in 0s, job 12s",
            why: "finished",
            detail: "unit ended",
            receipt: receipt(127, "the hand gave up", ""),
            want: Some(("agent_exit", None)),
        },
        Case {
            name: "bash: line 1: pi: command not found",
            why: "failed",
            detail: "bash: line 1: pi: command not found",
            receipt: receipt(1, "", ""),
            want: Some(("shell_marker", Some("bash"))),
        },
        Case {
            name: "receipt facts section with $ pi --version  (exit 127)",
            why: "failed",
            detail: "",
            receipt: facts_receipt(),
            want: Some(("facts_exit", Some("pi"))),
        },
        Case {
            name: "index.lock File exists",
            why: "failed",
            detail: "Unable to create '/repo/worktrees/s1/.git/index.lock': File exists.",
            receipt: receipt(1, "", ""),
            want: Some(("shell_marker", None)),
        },
        Case {
            name: "push outcome checkout_failed",
            why: "finished",
            detail: "checkout_failed",
            receipt: receipt(0, "", ""),
            want: Some(("push_outcome", None)),
        },
        Case {
            name: "stderr section line HTTP 503 Service Unavailable",
            why: "failed",
            detail: "",
            receipt: receipt(
                1,
                "the hand gave up",
                "HTTP 503 Service Unavailable\nretrying",
            ),
            want: Some(("endpoint", None)),
        },
        Case {
            name: "negative: agent exit 1",
            why: "failed",
            detail: "",
            receipt: receipt(1, "proof failed: expected 2 files, got 1", ""),
            want: None,
        },
        Case {
            name: "negative: a proof-failure text",
            why: "failed",
            detail: "proof failed: expected 2 files, got 1",
            receipt: receipt(0, "", ""),
            want: None,
        },
        Case {
            name: "negative: agent message mentions command not found",
            why: "failed",
            detail: "unit exited 2",
            receipt: receipt(
                2,
                "the unit hit pi: command not found and called it a unit bug",
                "",
            ),
            want: None,
        },
        Case {
            name: "negative: no_changes with clean facts",
            why: "finished",
            detail: "no_changes",
            receipt: receipt(0, "", "$ cargo test --lib pump  (exit 0)"),
            want: None,
        },
    ]
}

#[test]
fn classify_table_over_incident_evidence() {
    for case in cases() {
        let got = classify(case.why, case.detail, &case.receipt);
        match (got, case.want) {
            (None, None) => {}
            (Some(failure), Some((class, command))) => {
                assert_eq!(failure.class, class, "case {}", case.name);
                assert_eq!(failure.command.as_deref(), command, "case {}", case.name);
                assert!(
                    !failure.detail.contains('\n'),
                    "case {}: detail must be one line",
                    case.name
                );
                assert!(
                    failure.detail.starts_with(class),
                    "case {}: detail names the class: {}",
                    case.name,
                    failure.detail
                );
            }
            (got, want) => panic!("case {}: classify gave {got:?}, wanted {want:?}", case.name),
        }
    }
}

#[test]
fn failure_detail_truncates_evidence_to_200_chars() {
    let long = "x".repeat(500);
    let failure =
        classify("failed", &format!("bash: {long}: command not found"), "").expect("classified");
    let evidence = failure
        .detail
        .strip_prefix("shell_marker: ")
        .expect("prefix");
    assert_eq!(evidence.chars().count(), 201);
}

#[test]
fn sections_tail_joins_stderr_and_runner_log() {
    let text = "pool result j1: failed\n\n--- receipt\nunit said things\n\n--- stderr tail\nboom\n\n--- runner log tail\nlog line";
    let tail = sections_tail(text);
    assert!(tail.contains("boom"));
    assert!(tail.contains("log line"));
    assert!(!tail.contains("unit said things"));
}

#[test]
fn sections_tail_of_plain_message_is_empty() {
    assert_eq!(sections_tail("just the hand message, no sections"), "");
}

#[test]
fn missing_tool_by_code_and_text() {
    assert!(missing_tool(126, ""));
    assert!(missing_tool(127, ""));
    assert!(missing_tool(0, "bash: line 1: pi: command not found"));
    assert!(!missing_tool(1, "proof failed"));
    assert!(!missing_tool(0, "all green"));
}

#[test]
fn constants_match_the_plan() {
    assert_eq!(HOST_FAIL_WINDOW, 300);
    assert_eq!(HOST_PAUSE_AFTER, 2);
    assert_eq!(HOST_PUSH_OUTCOMES.len(), 10);
    assert!(HOST_PUSH_OUTCOMES.contains(&"checkout_failed"));
    assert!(HOST_PUSH_OUTCOMES.contains(&"target_worktree_missing"));
}
