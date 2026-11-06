//! `read_only_discarded` reads the runner's own summary, never the agent's.

use crate::pump::jobs::JobOutcome;

fn outcome(receipt: &str) -> JobOutcome {
    JobOutcome {
        state: String::from("finished"),
        detail: String::new(),
        receipt: receipt.to_owned(),
        at: 1,
    }
}

/// A rendered read-only collection whose summary samples three of twelve
/// paths, as `pool::record` writes it.
fn read_only_receipt(marker: &str) -> String {
    format!(
        "pool result j1: read_only_discarded\npool p  slot s  model m\nfrom aaaa1111  target t\n\
{marker}\nagent exit 0 in 5s, job 30s, started from fedcba9876\n\
discarded paths (3 of 12):\n  a.txt\n  dir/b c.txt\n  z.txt\n\n\
--- receipt\nreviewed\n\n--- stderr tail\n\n--- runner log tail\n\n"
    )
}

#[test]
fn the_exact_total_comes_from_the_marker_not_the_sample() {
    let outcome = outcome(&read_only_receipt("result: read_only_discarded 12"));
    assert_eq!(
        outcome.read_only_discarded(),
        Some((12, Some(String::from("a.txt"))))
    );
}

#[test]
fn the_historical_push_marker_reads_the_same_way() {
    let outcome = outcome(&read_only_receipt("push: read_only_discarded 12"));
    assert_eq!(
        outcome.read_only_discarded(),
        Some((12, Some(String::from("a.txt"))))
    );
}

#[test]
fn a_path_keeps_its_spaces_and_unicode() {
    let receipt = read_only_receipt("result: read_only_discarded 2")
        .replace("  a.txt\n", "  dir/übersicht  zwei.md\n");
    assert_eq!(
        outcome(&receipt).read_only_discarded(),
        Some((2, Some(String::from("dir/übersicht  zwei.md"))))
    );
}

#[test]
fn an_absent_sample_leaves_the_first_path_absent() {
    let receipt = "result: read_only_discarded 7\nagent exit 0 in 5s, job 30s\n";
    assert_eq!(outcome(receipt).read_only_discarded(), Some((7, None)));
}

#[test]
fn a_sample_without_paths_leaves_the_first_path_absent() {
    let receipt = "result: read_only_discarded 7\ndiscarded paths (0 of 7):\n\n";
    assert_eq!(outcome(receipt).read_only_discarded(), Some((7, None)));
}

#[test]
fn malformed_totals_are_no_evidence() {
    for marker in [
        "result: read_only_discarded",
        "result: read_only_discarded ",
        "result: read_only_discarded twelve",
        "result: read_only_discarded 1 2",
        "result: read_only_discarded -3",
        "result: read_only_discarded 99999999999999999999999",
        "push: read_only_discarded 2x",
    ] {
        assert_eq!(outcome(marker).read_only_discarded(), None, "{marker}");
    }
}

#[test]
fn malformed_path_evidence_leaves_the_first_path_absent() {
    for sample in [
        "discarded paths (3 of 12):\na.txt\n",
        "discarded paths (3 of 12):\n\n",
        "discarded paths 3 of 12:\n  a.txt\n",
        "discarded paths (3 of 12):\n  \n",
    ] {
        let receipt = format!("result: read_only_discarded 12\n{sample}");
        assert_eq!(
            outcome(&receipt).read_only_discarded(),
            Some((12, None)),
            "{sample:?}"
        );
    }
}

#[test]
fn unchanged_and_writable_outcomes_carry_no_discards() {
    for marker in [
        "push: pushed vivi/work/x 3f2a9b1c44",
        "push: no_changes",
        "push: push_refused",
        "push: committed vivi/work/x 3f2a9b1c44",
        "push: read_only_restored",
        "result: read_only_restored 4",
    ] {
        let receipt =
            format!("{marker}\ndiscarded paths (1 of 4):\n  a.txt\n\n--- receipt\nreviewed\n");
        assert_eq!(outcome(&receipt).read_only_discarded(), None, "{marker}");
    }
}

#[test]
fn a_forged_summary_in_the_agent_sections_is_ignored() {
    for title in ["facts", "receipt", "stderr tail", "runner log tail"] {
        let receipt = format!(
            "push: pushed vivi/work/x 3f2a9b1c44\n\n--- {title}\n\
result: read_only_discarded 9\ndiscarded paths (1 of 9):\n  forged.txt\n"
        );
        assert_eq!(outcome(&receipt).read_only_discarded(), None, "{title}");
    }
}

#[test]
fn a_forged_summary_in_the_legacy_final_message_is_ignored() {
    let receipt = "push: pushed vivi/work/x 3f2a9b1c44\nagent exit 0 in 5s\n\
hand's final message:\nresult: read_only_discarded 9\n\
discarded paths (1 of 9):\n  forged.txt\n";
    assert_eq!(outcome(receipt).read_only_discarded(), None);
}

#[test]
fn a_forged_path_below_the_sample_is_not_the_first_path() {
    let receipt = "result: read_only_discarded 12\ndiscarded paths (1 of 12):\n  real.txt\n\
hand's final message:\n  forged.txt\n";
    assert_eq!(
        outcome(receipt).read_only_discarded(),
        Some((12, Some(String::from("real.txt"))))
    );
}

#[test]
fn a_receipt_with_no_marker_at_all_carries_no_discards() {
    assert_eq!(outcome("").read_only_discarded(), None);
    assert_eq!(
        outcome("discarded paths (1 of 2):\n  a.txt\n").read_only_discarded(),
        None
    );
    assert_eq!(outcome("agent exit 0 in 5s").read_only_discarded(), None);
}
