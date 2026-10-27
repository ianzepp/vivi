use std::collections::BTreeMap;

use super::*;
use crate::pool::collect::{Collected, JobResult};

fn record(job: &str) -> DispatchRecord {
    DispatchRecord {
        target: None,
        repository: None,
        pool: "p".into(),
        need: None,
        kind: None,
        slot: "p-slot-1".into(),
        machine: "m1".into(),
        job: job.into(),
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
    }
}

fn event(handle: &str, kind: &str, note: String) -> PoolEventRow {
    PoolEventRow {
        handle: handle.into(),
        event_type: kind.into(),
        note,
    }
}

fn dispatched(handle: &str, job: &str) -> PoolEventRow {
    event(handle, "pool_dispatched", dispatch_note(&record(job)))
}

#[test]
fn a_dispatch_is_open_until_a_collected_event_follows() {
    let events = vec![
        dispatched("aaaa1111", "j1"),
        dispatched("bbbb2222", "j2"),
        event("aaaa1111", "pool_collected", "{}".into()),
    ];
    let open = fold_open(&events);
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].task, "bbbb2222");
    assert_eq!(open[0].record.job, "j2");
}

#[test]
fn a_task_can_be_dispatched_again_after_collection() {
    let events = vec![
        dispatched("aaaa1111", "j1"),
        event("aaaa1111", "pool_collected", "{}".into()),
        dispatched("aaaa1111", "j2"),
    ];
    let open = fold_open(&events);
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].record.job, "j2");
}

#[test]
fn unparseable_notes_are_not_dispatches() {
    let events = vec![event("aaaa1111", "pool_dispatched", "not json".into())];
    assert!(fold_open(&events).is_empty());
}

#[test]
fn dispatch_note_round_trips() {
    let r = record("j1");
    let back: DispatchRecord = serde_json::from_str(&dispatch_note(&r)).unwrap();
    assert_eq!(back, r);
    assert!(dispatch_text(&r).contains("branch factory/a"));
}

#[test]
fn older_dispatch_receipts_leave_need_and_kind_unavailable() {
    let mut old = serde_json::to_value(record("j1")).unwrap();
    old.as_object_mut().unwrap().remove("need");
    old.as_object_mut().unwrap().remove("kind");
    let back: DispatchRecord = serde_json::from_value(old).unwrap();
    assert_eq!(back.need, None);
    assert_eq!(back.kind, None);
}

#[test]
fn a_record_round_trips_with_and_without_extra_refs() {
    let mut r = record("j1");
    r.with = vec!["review/aaa@aaaaaaaaaaaa".into(), "b@bbbbbbbbbbbb".into()];
    let back: DispatchRecord = serde_json::from_str(&dispatch_note(&r)).unwrap();
    assert_eq!(back, r);
    let none = record("j1");
    assert!(
        !dispatch_note(&none).contains("\"with\""),
        "a record with none carries no field"
    );
    let old: DispatchRecord = serde_json::from_str(
        "{\"slot\":\"s\",\"machine\":\"m\",\"job\":\"j\",\"model\":\"m/x\",\
         \"from\":\"main\",\"branch\":\"b\",\"start\":\"c\",\"budget\":1,\"at\":0}",
    )
    .unwrap();
    assert!(
        old.with.is_empty(),
        "a record made before extra refs existed reads as none"
    );
}

fn finished() -> Outcome {
    finished_with(&[])
}

/// A finished write job whose result carries `extra` key=value lines.
fn finished_with(extra: &[(&str, &str)]) -> Outcome {
    let mut fields = BTreeMap::from([
        ("job".to_string(), "j1".to_string()),
        ("push".to_string(), "pushed".to_string()),
        ("branch".to_string(), "factory/a".to_string()),
        ("sha".to_string(), "b".repeat(40)),
        ("agent_rc".to_string(), "0".to_string()),
    ]);
    fields.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    Outcome::Finished(Collected {
        result: JobResult { fields },
        receipt: "did the thing\n".into(),
        stderr: String::new(),
        log: "from main at abc\n".into(),
    })
}

#[test]
fn collected_records_carry_the_result_and_the_receipt() {
    let d = OpenDispatch {
        task: "aaaa1111".into(),
        record: record("j1"),
    };
    let note: serde_json::Value = serde_json::from_str(&collected_note(&d, &finished())).unwrap();
    assert_eq!(note["outcome"], "finished");
    assert_eq!(note["push"], "pushed");
    assert_eq!(note["job"], "j1");
    let text = collected_text(&d, &finished());
    assert!(text.contains("pool result j1: finished"));
    assert!(text.contains("pushed factory/a bbbbbbbbbb"));
    assert!(text.contains("--- receipt\ndid the thing"));
    assert!(text.contains("--- runner log tail"));
    assert!(!text.contains("--- stderr"), "empty sections are left out");
}

#[test]
fn timeout_and_lost_say_what_happened() {
    let d = OpenDispatch {
        task: "aaaa1111".into(),
        record: record("j1"),
    };
    assert!(collected_text(&d, &Outcome::Timeout).contains("slot was stopped"));
    assert!(collected_text(&d, &Outcome::Lost).contains("without writing a result"));
    let note: serde_json::Value =
        serde_json::from_str(&collected_note(&d, &Outcome::Timeout)).unwrap();
    assert_eq!(note["outcome"], "timeout");
}

fn read_only_record(job: &str) -> DispatchRecord {
    DispatchRecord {
        branch: String::new(),
        read_only: true,
        ..record(job)
    }
}

fn read_only_outcome(extra: &[(&str, &str)]) -> Outcome {
    let mut fields = BTreeMap::from([
        ("job".to_string(), "j1".to_string()),
        ("branch".to_string(), String::new()),
        ("sha".to_string(), String::new()),
        ("agent_rc".to_string(), "0".to_string()),
        ("agent_s".to_string(), "3".to_string()),
        ("job_s".to_string(), "5".to_string()),
        ("start".to_string(), "c".repeat(40)),
    ]);
    fields.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    Outcome::Finished(Collected {
        result: JobResult { fields },
        receipt: "read it all\n".into(),
        stderr: String::new(),
        log: String::new(),
    })
}

#[test]
fn a_record_made_before_read_only_jobs_reads_as_a_normal_dispatch() {
    let mut value: serde_json::Value = serde_json::from_str(&dispatch_note(&record("j1"))).unwrap();
    value.as_object_mut().unwrap().remove("read_only");
    let back: DispatchRecord = serde_json::from_value(value).unwrap();
    assert!(!back.read_only);
}

#[test]
fn a_read_only_dispatch_round_trips_and_its_text_says_so() {
    let r = read_only_record("j1");
    let back: DispatchRecord = serde_json::from_str(&dispatch_note(&r)).unwrap();
    assert_eq!(back, r);
    let text = dispatch_text(&r);
    assert!(
        text.contains("read-only (nothing is committed or pushed)"),
        "{text}"
    );
    assert!(!text.contains("branch"), "{text}");
}

#[test]
fn an_unchanged_read_only_result_records_the_outcome_and_the_receipt() {
    let d = OpenDispatch {
        task: "aaaa1111".into(),
        record: read_only_record("j1"),
    };
    let outcome = read_only_outcome(&[("push", "read_only_unchanged")]);
    let note: serde_json::Value = serde_json::from_str(&collected_note(&d, &outcome)).unwrap();
    assert_eq!(note["push"], "read_only_unchanged");
    assert_eq!(note["read_only"], true);
    assert!(note.get("discarded").is_none());
    let text = collected_text(&d, &outcome);
    assert!(text.contains("from main  read-only"), "{text}");
    assert!(text.contains("result: read_only_unchanged"), "{text}");
    assert!(!text.contains("push:"), "{text}");
    assert!(!text.contains("discarded paths"), "{text}");
    assert!(text.contains("--- receipt\nread it all"), "{text}");
}

#[test]
fn a_discarding_read_only_result_lists_the_paths_in_order() {
    let d = OpenDispatch {
        task: "aaaa1111".into(),
        record: read_only_record("j1"),
    };
    let outcome = read_only_outcome(&[
        ("push", "read_only_discarded"),
        ("discarded_count", "12"),
        ("discarded_path_1", "a.txt"),
        ("discarded_path_2", "dir/b c.txt"),
        ("discarded_path_10", "z.txt"),
    ]);
    let note: serde_json::Value = serde_json::from_str(&collected_note(&d, &outcome)).unwrap();
    assert_eq!(note["push"], "read_only_discarded");
    assert_eq!(note["discarded_count"], "12");
    assert_eq!(
        note["discarded"],
        serde_json::json!(["a.txt", "dir/b c.txt", "z.txt"]),
        "numeric order, not text order"
    );
    assert!(note.get("discarded_path_1").is_none(), "{note}");
    let text = collected_text(&d, &outcome);
    assert!(text.contains("result: read_only_discarded 12"), "{text}");
    assert!(
        text.contains("discarded paths (3 of 12):\n  a.txt\n  dir/b c.txt\n  z.txt"),
        "{text}"
    );
}

#[test]
fn a_restoring_result_lists_the_paths_in_order_and_names_them_in_the_text() {
    let d = OpenDispatch {
        task: "aaaa1111".into(),
        record: record("j1"),
    };
    let outcome = finished_with(&[
        ("restored_count", "12"),
        ("restored_path_1", "runtime/llvm/Cargo.lock"),
        ("restored_path_2", "dir/b c.txt"),
        ("restored_path_10", "z.txt"),
    ]);
    let note: serde_json::Value = serde_json::from_str(&collected_note(&d, &outcome)).unwrap();
    assert_eq!(note["restored_count"], "12");
    assert_eq!(
        note["restored"],
        serde_json::json!(["runtime/llvm/Cargo.lock", "dir/b c.txt", "z.txt"]),
        "numeric order, not text order"
    );
    assert!(note.get("restored_path_1").is_none(), "{note}");
    let text = collected_text(&d, &outcome);
    assert!(
        text.contains(
            "restored 3 paths before commit: runtime/llvm/Cargo.lock, dir/b c.txt, z.txt"
        ),
        "{text}"
    );
}

#[test]
fn a_result_without_restored_paths_has_no_restored_key_or_line() {
    let d = OpenDispatch {
        task: "aaaa1111".into(),
        record: record("j1"),
    };
    let note: serde_json::Value = serde_json::from_str(&collected_note(&d, &finished())).unwrap();
    assert!(note.get("restored").is_none());
    assert!(!collected_text(&d, &finished()).contains("restored"));
}

#[test]
fn a_result_with_extra_refs_lists_each_then_the_depth_before_the_facts() {
    let d = OpenDispatch {
        task: "aaaa1111".into(),
        record: record("j1"),
    };
    let commit = "c".repeat(40);
    let outcome = finished_with(&[
        ("restored_path_1", "Cargo.lock"),
        ("with_count", "2"),
        ("with_1_ref", "review"),
        ("with_1_local", "refs/remotes/origin/review"),
        ("with_1_sha", &"1".repeat(40)),
        ("with_1_merge_base", &"2".repeat(40)),
        ("with_2_ref", &commit),
        ("with_2_local", &format!("refs/vivi/with/{commit}")),
        ("with_2_sha", &"3".repeat(40)),
        ("with_2_merge_base", "none"),
        ("fetch_depth", "200"),
    ]);
    let text = collected_text(&d, &outcome);
    assert!(
        text.contains(&format!(
            "restored 1 path before commit: Cargo.lock\n\
             extra ref review -> refs/remotes/origin/review at 111111111111 \
             (merge base 222222222222)\n\
             extra ref {commit} -> refs/vivi/with/{commit} at 333333333333 \
             (merge base none)\n\
             fetched at depth 200"
        )),
        "{text}"
    );
    let depth = text.find("fetched at depth 200").unwrap();
    assert!(depth < text.find("--- receipt").unwrap(), "{text}");
}

#[test]
fn a_result_without_extra_refs_has_no_with_lines_or_depth() {
    let d = OpenDispatch {
        task: "aaaa1111".into(),
        record: record("j1"),
    };
    let text = collected_text(&d, &finished());
    assert!(!text.contains("extra ref"), "{text}");
    assert!(!text.contains("merge base"), "{text}");
    assert!(!text.contains("fetched at depth"), "{text}");
}

#[test]
fn a_worktree_style_result_lists_refs_without_merge_base_or_depth() {
    let d = OpenDispatch {
        task: "aaaa1111".into(),
        record: record("j1"),
    };
    let outcome = finished_with(&[
        ("with_count", "1"),
        ("with_1_ref", "B"),
        ("with_1_local", "B"),
        ("with_1_sha", &"b".repeat(40)),
    ]);
    let text = collected_text(&d, &outcome);
    assert!(text.contains("extra ref B -> B at bbbbbbbbbbbb"), "{text}");
    assert!(!text.contains("merge base"), "{text}");
    assert!(!text.contains("fetched at depth"), "{text}");
}

#[test]
fn a_count_above_the_limit_is_clamped_and_missing_keys_render_as_dashes() {
    let d = OpenDispatch {
        task: "aaaa1111".into(),
        record: record("j1"),
    };
    for count in ["99", "many"] {
        let outcome = finished_with(&[("with_count", count)]);
        let text = collected_text(&d, &outcome);
        assert_eq!(text.matches("extra ref ").count(), WITH_LIMIT, "{text}");
        assert!(text.contains("\nextra ref - -> - at -\n"), "{text}");
    }
}

#[test]
fn the_note_keeps_the_extra_ref_keys_flat() {
    let d = OpenDispatch {
        task: "aaaa1111".into(),
        record: record("j1"),
    };
    let outcome = finished_with(&[
        ("with_count", "1"),
        ("with_1_ref", "review"),
        ("with_1_local", "refs/remotes/origin/review"),
        ("with_1_sha", &"1".repeat(40)),
        ("with_1_merge_base", &"2".repeat(40)),
        ("fetch_depth", "200"),
    ]);
    let note: serde_json::Value = serde_json::from_str(&collected_note(&d, &outcome)).unwrap();
    assert_eq!(note["with_count"], "1");
    assert_eq!(note["with_1_sha"], "1".repeat(40));
    assert_eq!(note["fetch_depth"], "200");
}

#[test]
fn facts_are_in_the_json_note_and_shown_before_the_receipt_not_as_flat_keys() {
    let d = OpenDispatch {
        task: "aaaa1111".into(),
        record: read_only_record("j1"),
    };
    let outcome = read_only_outcome(&[
        ("push", "read_only_unchanged"),
        ("fact_1_cmd", "gpu-query --csv"),
        ("fact_1_exit", "0"),
        ("fact_1_out", r"Some GPU, 550.1\n8 GiB"),
        ("fact_2_cmd", "missing-tool"),
        ("fact_2_exit", "127"),
        ("fact_2_out", "missing-tool: not found"),
    ]);
    let note: serde_json::Value = serde_json::from_str(&collected_note(&d, &outcome)).unwrap();
    assert_eq!(
        note["facts"],
        serde_json::json!([
            {"command": "gpu-query --csv", "exit": 0, "output": "Some GPU, 550.1\n8 GiB"},
            {"command": "missing-tool", "exit": 127, "output": "missing-tool: not found"},
        ])
    );
    assert!(note.get("fact_1_cmd").is_none(), "{note}");
    let text = collected_text(&d, &outcome);
    let facts = text.find("--- facts\n").unwrap();
    assert!(facts < text.find("--- receipt").unwrap(), "{text}");
    assert!(
        text.contains("$ gpu-query --csv  (exit 0)\n  Some GPU, 550.1\n  8 GiB\n$ missing-tool  (exit 127)\n  missing-tool: not found"),
        "{text}"
    );
}

#[test]
fn a_result_without_facts_has_no_facts_key_and_no_heading() {
    let d = OpenDispatch {
        task: "aaaa1111".into(),
        record: record("j1"),
    };
    let note: serde_json::Value = serde_json::from_str(&collected_note(&d, &finished())).unwrap();
    assert!(note.get("facts").is_none());
    assert!(!collected_text(&d, &finished()).contains("--- facts"));
}

#[test]
fn abandoned_pods_are_stated_with_an_approximate_cost_and_only_when_there_are_any() {
    let none = record("j");
    assert_eq!(abandoned_text(&none), "");
    assert!(!dispatch_text(&none).contains("abandoned"));
    assert!(
        !dispatch_note(&none).contains("abandoned"),
        "a record with none is unchanged on disk"
    );
    let two = DispatchRecord {
        target: None,
        repository: None,
        abandoned: 2,
        abandoned_cost_micros: 123_456,
        ..record("j")
    };
    assert_eq!(
        abandoned_text(&two),
        "abandoned 2 pod(s) that did not become reachable, approx $0.12"
    );
    assert!(dispatch_text(&two).ends_with("approx $0.12"));
    let back: DispatchRecord = serde_json::from_str(&dispatch_note(&two)).unwrap();
    assert_eq!(back, two);
    let unpriced = DispatchRecord {
        abandoned_cost_micros: 0,
        ..two
    };
    assert!(abandoned_text(&unpriced).ends_with("price unknown"));
}

// ---- section splitting ------------------------------------------------------
//
// A reader of a pool result letter splits only on the section headings the
// pool renders. The round-trip regressions render letters with
// `collected_text` and split them back apart; the rest drive the splitter
// directly on hand-written shapes.

/// A finished outcome with the given section bodies.
fn finished_sections(receipt: &str, stderr: &str, log: &str) -> Outcome {
    Outcome::Finished(Collected {
        result: JobResult {
            fields: BTreeMap::from([
                ("job".to_string(), "j1".to_string()),
                ("push".to_string(), "pushed".to_string()),
                ("branch".to_string(), "factory/a".to_string()),
                ("sha".to_string(), "b".repeat(40)),
                ("agent_rc".to_string(), "0".to_string()),
            ]),
        },
        receipt: receipt.into(),
        stderr: stderr.into(),
        log: log.into(),
    })
}

/// The rendered letter of a finished result with the given section bodies.
fn letter(receipt: &str, stderr: &str, log: &str) -> String {
    let d = OpenDispatch {
        task: "aaaa1111".into(),
        record: record("j1"),
    };
    collected_text(&d, &finished_sections(receipt, stderr, log))
}

/// The sections of a letter as owned pairs, for whole-vector assertions.
fn section_pairs(text: &str) -> Vec<(&str, &str)> {
    split_sections(text).1.collect()
}

#[test]
fn a_split_round_trip_keeps_a_receipt_with_document_headings_whole() {
    let receipt = "done\n\n--- Begin document ---\ndoc body\n\n--- title\nafter the lookalike";
    let text = letter(receipt, "boom\n", "log line\n");
    let got = section_pairs(&text);
    assert_eq!(
        got,
        vec![
            (SECTION_RECEIPT, receipt),
            (SECTION_STDERR, "boom"),
            (SECTION_RUNNER_LOG, "log line"),
        ]
    );
}

#[test]
fn omitted_sections_are_absent_and_an_unfollowed_receipt_runs_to_the_end() {
    let receipt = "to the end\n\n--- and beyond\nstill mine\n";
    let text = letter(receipt, "", "");
    let got = section_pairs(&text);
    assert_eq!(
        got,
        vec![(SECTION_RECEIPT, "to the end\n\n--- and beyond\nstill mine")]
    );
    let text = letter("kept\n", "", "");
    let (head, _) = split_sections(&text);
    assert!(head.contains("pool result j1: finished"), "{head}");
}

#[test]
fn explicit_empty_sections_are_recognized_including_a_heading_at_eof() {
    let (head, sections) =
        split_sections("sum\n\n--- receipt\n\n--- stderr tail\n\n--- runner log tail");
    assert_eq!(head, "sum");
    assert_eq!(
        sections.collect::<Vec<_>>(),
        vec![
            (SECTION_RECEIPT, ""),
            (SECTION_STDERR, ""),
            (SECTION_RUNNER_LOG, ""),
        ]
    );
    let (head, sections) = split_sections("sum\n--- receipt");
    assert_eq!(head, "sum");
    assert_eq!(sections.collect::<Vec<_>>(), vec![(SECTION_RECEIPT, "")]);
    let (head, sections) = split_sections("");
    assert_eq!(head, "");
    assert_eq!(sections.count(), 0);
}

#[test]
fn lookalike_and_incomplete_heading_lines_stay_content() {
    let (_, sections) = split_sections(
        "sum\n\n--- receipt\n--- receipt extended\n---receipt\n--- receipt \n\
         --- stderr tailish\nmine\n\n--- stderr tail\nreal\n",
    );
    assert_eq!(
        sections.collect::<Vec<_>>(),
        vec![
            (
                SECTION_RECEIPT,
                "--- receipt extended\n---receipt\n--- receipt \n--- stderr tailish\nmine"
            ),
            (SECTION_STDERR, "real\n"),
        ]
    );
}

#[test]
fn repeated_and_backward_headings_inside_a_receipt_stay_content() {
    let (_, sections) = split_sections(
        "sum\n\n--- receipt\nmine\n--- receipt\nagain\n--- facts\nbackward\
         \nstill mine\n\n--- stderr tail\nerr\n\n--- runner log tail\nlog",
    );
    assert_eq!(
        sections.collect::<Vec<_>>(),
        vec![
            (
                SECTION_RECEIPT,
                "mine\n--- receipt\nagain\n--- facts\nbackward\nstill mine"
            ),
            (SECTION_STDERR, "err"),
            (SECTION_RUNNER_LOG, "log"),
        ]
    );
}

#[test]
fn a_heading_after_a_single_newline_still_starts_a_section() {
    let (_, sections) =
        split_sections("sum\n--- receipt\nmine\n--- stderr tail\nerr\n--- runner log tail\nlog");
    assert_eq!(
        sections.collect::<Vec<_>>(),
        vec![
            (SECTION_RECEIPT, "mine"),
            (SECTION_STDERR, "err"),
            (SECTION_RUNNER_LOG, "log"),
        ]
    );
}

#[test]
fn separation_beyond_a_blank_line_keeps_the_extra_newlines_as_content() {
    let (head, sections) = split_sections("sum\n\n\n--- receipt\nmine\n\n\n\n--- stderr tail\nerr");
    assert_eq!(head, "sum\n");
    assert_eq!(
        sections.collect::<Vec<_>>(),
        vec![(SECTION_RECEIPT, "mine\n\n"), (SECTION_STDERR, "err")]
    );
}
