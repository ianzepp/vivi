//! Tests for prior-attempt context capture and rendering: retention,
//! malformed-envelope prose, independent Unicode limits, section filtering,
//! unavailable evidence, and safe rendering.

use serde_json::{Value, json};

use super::{capture, render};
use crate::pump::jobs::{JobOutcome, JobSpec, PoolJob};

/// The original job id every capture names, resolved by the collector.
const SOURCE: &str = "orig-j1";

/// A finished repairer job with `receipt` as its collected receipt and a
/// task handle.
fn original(receipt: &str) -> PoolJob {
    let mut job = PoolJob::from(JobSpec::new(SOURCE, "repairer", "repairer", "s", "b"));
    job.task = Some("t9".to_owned());
    job.outcome = Some(JobOutcome {
        state: "finished".to_owned(),
        detail: String::new(),
        receipt: receipt.to_owned(),
        at: 0,
    });
    job
}

/// A full pool receipt for the original: hand `message` in the receipt
/// section and one stderr marker line.
fn receipt(message: &str) -> String {
    format!(
        "pool result j1: finished\npool p  slot s1  model m\n\
         agent exit 1 in 9s, job 12s, started from 123\n\n\
         --- receipt\n{message}\n\n--- stderr tail\nerror: boom\n"
    )
}

#[test]
fn capture_keeps_source_job_and_task_outside_cuts() {
    // 10 000 bytes of summary prose: the summary is cut, the id and task
    // above it are not part of any cut.
    let long = "word ".repeat(2_000);
    let job = original(&receipt(&format!("+++\nanswer = \"failed\"\n+++\n{long}")));
    let context = capture(SOURCE, Some(&job));
    assert_eq!(context["source_job"], json!(SOURCE));
    assert_eq!(context["task"], json!("t9"));
    assert_eq!(context["summary"]["truncated"], json!(true));
}

#[test]
fn capture_labels_the_summary_model_authored_and_strips_front_matter() {
    let message = "+++\nanswer = \"failed\"\n[[check]]\ncmd = \"cargo test\"\n\
                   result = \"fail\"\n+++\n\nthe flake bit again\n";
    let context = capture(SOURCE, Some(&original(&receipt(message))));
    let summary = &context["summary"];
    assert_eq!(summary["authored_by"], json!("model"));
    assert_eq!(summary["text"], json!("the flake bit again"));
    assert_eq!(summary["truncated"], json!(false));
    assert_eq!(summary["bytes"], json!("the flake bit again".len()));
    // The envelope front matter never leaks into the summary.
    let text = summary["text"].as_str().expect("summary text");
    assert!(!text.contains("+++"), "{text}");
    assert!(!text.contains("answer"), "{text}");
    assert!(!text.contains("check"), "{text}");
}

#[test]
fn capture_keeps_prose_when_the_envelope_is_malformed() {
    // A correction is precisely such a case: the original never produced a
    // well-formed envelope, so its available prose survives as written.
    let message = "+++\nanswer = \"failed\"\nthe fence never closes\n";
    let context = capture(SOURCE, Some(&original(&receipt(message))));
    let text = context["summary"]["text"].as_str().expect("summary text");
    assert!(text.contains("the fence never closes"), "{text}");
    assert!(text.contains("answer = \"failed\""), "{text}");
}

#[test]
fn capture_bounds_the_summary_at_a_utf8_safe_cut_with_an_explicit_marker() {
    // Each 'é' is two UTF-8 bytes: a naive byte cut would split one.
    let unit = "é".repeat(3_000);
    let context = capture(SOURCE, Some(&original(&receipt(&unit))));
    let text = context["summary"]["text"].as_str().expect("summary text");
    let (kept, tail) = text
        .split_once("\n[cut at 4096 bytes]")
        .unwrap_or_else(|| panic!("no explicit cut marker: {text:?}"));
    assert_eq!(tail, "");
    assert!(kept.len() <= 4096, "{} bytes", kept.len());
    assert_eq!(kept.len() % 2, 0, "cut mid-character");
    assert!(kept.chars().all(|c| c == 'é'), "{kept:?}");
}

#[test]
fn capture_bounds_summary_and_excerpt_each_at_their_own_independent_limit() {
    // The summary cap (4 KiB) and the excerpt's own cap are independent:
    // both parts truncate here, each on its own bound and metadata.
    let summary_unit = "é".repeat(3_000);
    let stderr_unit = "é".repeat(7_000); // 14 000 bytes, one line, no marker
    let receipt_text = format!(
        "pool result j1: finished\n\n--- receipt\n{summary_unit}\n\n\
         --- stderr tail\n{stderr_unit}\n"
    );
    let context = capture(SOURCE, Some(&original(&receipt_text)));
    let summary = &context["summary"];
    assert_eq!(summary["truncated"], json!(true));
    let kept = summary["text"].as_str().expect("summary text");
    assert!(kept.contains("\n[cut at 4096 bytes]"), "no summary cut");
    let failure = &context["failure"];
    assert_eq!(failure["mode"], json!("tail"));
    assert_eq!(failure["truncated"], json!(true));
    let excerpt = failure["text"].as_str().expect("excerpt text");
    assert!(excerpt.len() <= 12 * 1024, "{} bytes", excerpt.len());
    assert_eq!(excerpt.len() % 2, 0, "excerpt cut mid-character");
    // Each part's bound says nothing about the other: the summary cut named
    // its own 4096-byte limit while the excerpt reports the excerpt's cap.
    assert_ne!(summary["bytes"], failure["source_lines"]);
}

#[test]
fn capture_keeps_excerpt_metadata_from_the_receipt_sections() {
    let long = "word\n".repeat(2_000);
    let receipt_text = format!(
        "pool result j1: finished\n\n--- receipt\n{long}\n\n--- stderr tail\nerror: boom\n"
    );
    let context = capture(SOURCE, Some(&original(&receipt_text)));
    let failure = &context["failure"];
    assert_eq!(failure["mode"], json!("markers"));
    // 2 000 receipt lines, the join's empty line, the stderr marker line.
    assert_eq!(failure["source_lines"], json!(2_002));
    // The marker window keeps the last three receipt lines and the marker.
    assert_eq!(failure["kept_lines"], json!(4));
    assert_eq!(failure["omitted_lines"], json!(1_998));
    assert_eq!(failure["truncated"], json!(false));
    let text = failure["text"].as_str().expect("excerpt text");
    assert!(text.contains("error: boom"), "{text}");
    assert!(!text.contains("word word"), "{text}");
}

#[test]
fn capture_never_manufactures_a_summary_from_pool_summary_or_log_sections() {
    // No `--- receipt` section: the hand printed nothing. The pool summary
    // and the runner log are machine facts and must not become the summary;
    // only the failure excerpt may read the stderr section.
    let receipt_text = "pool result j1: finished\npool p  slot s1  model m\n\
         agent exit 1 in 9s, job 12s\n\n--- stderr tail\nerror: boom\n\
         --- runner log tail\nsummary: the work is done\n";
    let context = capture(SOURCE, Some(&original(&receipt_text)));
    assert_eq!(context["summary"], Value::Null);
    assert_eq!(context["failure"]["mode"], json!("markers"));
    let text = context["failure"]["text"].as_str().expect("excerpt text");
    assert!(text.contains("error: boom"), "{text}");
    assert!(!text.contains("the work is done"), "{text}");
}

#[test]
fn capture_marks_a_missing_original_job_explicitly_unavailable() {
    let context = capture(SOURCE, None);
    assert_eq!(context["source_job"], json!(SOURCE));
    assert_eq!(context["task"], Value::Null);
    assert_eq!(context["summary"], Value::Null);
    assert_eq!(context["failure"], Value::Null);
}

#[test]
fn capture_marks_a_missing_outcome_explicitly_unavailable() {
    let job = PoolJob::from(JobSpec::new(SOURCE, "repairer", "repairer", "s", "b"));
    let context = capture(SOURCE, Some(&job));
    assert_eq!(context["task"], Value::Null);
    assert_eq!(context["summary"], Value::Null);
    assert_eq!(context["failure"], Value::Null);
}

#[test]
fn capture_marks_an_empty_hand_message_explicitly_unavailable() {
    let context = capture(SOURCE, Some(&original(&receipt("   "))));
    assert_eq!(context["summary"], Value::Null);
    // The receipt and stderr sections still stand as failure evidence.
    assert_eq!(context["failure"]["mode"], json!("markers"));
}

#[test]
fn render_of_absent_historical_context_is_safe() {
    assert_eq!(render(None), "none");
    assert_eq!(render(Some(&Value::Null)), "none");
    assert_eq!(render(Some(&json!("garbage"))), "none");
}

#[test]
fn render_shows_the_model_authored_summary_and_the_failure_evidence() {
    let message = "+++\nanswer = \"failed\"\n+++\nthe flake bit again";
    let context = capture(SOURCE, Some(&original(&receipt(message))));
    let text = render(Some(&context));
    assert!(text.contains("original job orig-j1 on task t9"), "{text}");
    assert!(text.contains("summary (model-authored):"), "{text}");
    assert!(text.contains("  the flake bit again\n"), "{text}");
    assert!(text.contains("failure:\n"), "{text}");
    assert!(text.contains("  error: boom\n"), "{text}");
}

#[test]
fn render_names_unavailable_parts() {
    let context = capture(SOURCE, None);
    let text = render(Some(&context));
    assert!(text.contains("original job orig-j1\n"), "{text}");
    assert!(text.contains("summary: none (unavailable)\n"), "{text}");
    assert!(text.contains("failure: none (unavailable)\n"), "{text}");
}

#[test]
fn render_of_an_unexpected_value_shape_is_safe() {
    let text = render(Some(&json!({"summary": 3})));
    assert!(text.contains("summary:\n  (none)\n"), "{text}");
    assert!(text.contains("failure: none (unavailable)\n"), "{text}");
}
