//! Tests for [`crate::pump::brief`].

use std::fs;

use tempfile::tempdir;

use crate::pump::brief::{DEFAULT_HAND_RULES, brief_for, gate_brief, hand_rules};
use crate::pump::config::PumpConfig;
use crate::pump::host::test_ctx;
use crate::pump::state::{Attempt, Unit};

/// A [`PumpConfig`] with a distinguishable gate command.
fn cfg_with_gate(command: &str) -> PumpConfig {
    let mut cfg = PumpConfig::default();
    cfg.gate.command = command.to_owned();
    cfg
}

/// A small unit with a title, brief, two scope files and two proof commands.
fn sample_unit() -> Unit {
    Unit {
        title: "own the rows".to_owned(),
        brief: "fix the thing described here".to_owned(),
        write_scope: vec!["src/a.rs".to_owned(), "src/b.rs".to_owned()],
        proof: vec![
            "cargo test --lib a".to_owned(),
            "cargo test --lib b".to_owned(),
        ],
        ..Unit::default()
    }
}

#[test]
fn the_default_rules_name_no_project_specific_tool() {
    for banned in [
        "Faber", "Rust", "rust", "scripta", "stage", "GPU", "ledger", "mirrored", "cargo",
        "Python", "python", "pump",
    ] {
        assert!(
            !DEFAULT_HAND_RULES.contains(banned),
            "default rules mention {banned}"
        );
    }
    // The generic bullets survive the port.
    assert!(DEFAULT_HAND_RULES.contains("Read AGENTS.md"));
    assert!(DEFAULT_HAND_RULES.contains("VERDICT: no-defect"));
    assert!(DEFAULT_HAND_RULES.contains("Change only the files in WRITE SCOPE"));
    assert!(DEFAULT_HAND_RULES.contains("git commit"));
    assert!(DEFAULT_HAND_RULES.contains("Run every PROOF command and make it pass"));
    assert!(DEFAULT_HAND_RULES.contains("cfg-gate"));
    assert!(DEFAULT_HAND_RULES.contains("Hard stop"));
}

#[test]
fn hand_rules_reads_the_configured_file() {
    let dir = tempdir().unwrap();
    let rules = dir.path().join("rules.txt");
    fs::write(&rules, "house rules: keep it tidy").unwrap();
    let cfg = PumpConfig {
        hand_rules: Some(rules.to_string_lossy().into_owned()),
        ..PumpConfig::default()
    };
    let (ctx, _host) = test_ctx(dir.path(), cfg);
    assert_eq!(hand_rules(&ctx), "house rules: keep it tidy");
}

#[test]
fn a_relative_hand_rules_path_resolves_against_the_project_root() {
    let dir = tempdir().unwrap();
    let project = dir.path().join("project");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("rules.txt"), "project-relative rules").unwrap();
    let cfg = PumpConfig {
        hand_rules: Some(String::from("rules.txt")),
        ..PumpConfig::default()
    };
    let (ctx, _host) = test_ctx(dir.path(), cfg);
    assert_eq!(hand_rules(&ctx), "project-relative rules");
}

#[test]
fn hand_rules_falls_back_to_the_default_when_the_file_is_missing() {
    let dir = tempdir().unwrap();
    let cfg = PumpConfig {
        hand_rules: Some(dir.path().join("absent.txt").to_string_lossy().into_owned()),
        ..PumpConfig::default()
    };
    let (ctx, _host) = test_ctx(dir.path(), cfg);
    assert_eq!(hand_rules(&ctx), DEFAULT_HAND_RULES);
}

#[test]
fn brief_for_lists_scope_and_proof_in_order() {
    let dir = tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let unit = sample_unit();
    let text = brief_for(&ctx, "56f1d749-2", &unit);
    let title = text.find("UNIT 56f1d749-2: own the rows").unwrap();
    let brief = text.find("fix the thing described here").unwrap();
    let scope = text.find("WRITE SCOPE:").unwrap();
    let proof = text.find("PROOF (each must exit 0):").unwrap();
    assert!(title < brief && brief < scope && scope < proof);
    assert!(text.contains("\n- src/a.rs\n- src/b.rs\n"));
    assert!(text.contains("\n- `cargo test --lib a`\n- `cargo test --lib b`\n"));
    assert!(text.ends_with(
        "done_when: every PROOF command exits 0 on your checkout and the checks named in \
         the rules above pass, or the reply ends with `VERDICT: no-defect` and evidence"
    ));
    assert!(!text.contains("A PREVIOUS ATTEMPT"));
}

#[test]
fn brief_for_an_empty_proof_says_shared_checks_only() {
    let dir = tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let unit = Unit {
        proof: Vec::new(),
        ..sample_unit()
    };
    let text = brief_for(&ctx, "56f1d749-2", &unit);
    assert!(
        text.contains("PROOF (each must exit 0):\n- (no unit proof; the shared checks only)\n")
    );
}

#[test]
fn brief_for_a_retry_carries_the_last_failure_cut_to_1800_chars() {
    let dir = tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    // 900 two-byte characters, then ASCII: a naive byte cut at 1800 lands
    // mid-character, so the brief must move the cut forward.
    let detail = format!("{}!{}", "\u{e9}".repeat(900), "y".repeat(500));
    let unit = Unit {
        history: vec![Attempt {
            attempt: 1,
            model: None,
            why: "proof-failed".to_owned(),
            detail: detail.clone(),
            at: 0,
        }],
        ..sample_unit()
    };
    let text = brief_for(&ctx, "56f1d749-2", &unit);
    assert!(
        text.contains(
            "A PREVIOUS ATTEMPT AT THIS UNIT FAILED (proof-failed). Its work was \
             discarded; you start clean. What went wrong:"
        ),
        "failure header missing"
    );
    let marker = "What went wrong:\n\n";
    let tail = &text[text.find(marker).unwrap() + marker.len()..];
    let body = tail
        .strip_suffix(
            "\n\ndone_when: every PROOF command exits 0 on your checkout and the \
                       checks named in the rules above pass, or the reply ends with \
                       `VERDICT: no-defect` and evidence",
        )
        .unwrap();
    // Byte 1800 from the end is byte 501, inside a two-byte character; the
    // cut moves to 502, keeping 649 of the accented characters.
    let expected = format!("{}!{}", "\u{e9}".repeat(649), "y".repeat(500));
    assert_eq!(body, expected);
    assert!(body.len() <= 1800);
    assert!(!text.contains(&detail));
}

#[test]
fn gate_brief_names_the_gate_command_and_the_marker_block() {
    let dir = tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), cfg_with_gate("make gate"));
    let text = gate_brief(&ctx);
    assert!(text.starts_with(
        "Measurement only. Change nothing in the repository; scratch files \
                          go under /tmp."
    ));
    assert!(text.contains("    make gate > /tmp/pump-gate.log 2>&1; echo \"rc=$?\""));
    assert!(text.contains("FAILING-BEGIN\n<crate> <test name>\nFAILING-END"));
    assert!(text.contains("BUILD-FAILED"));
    assert!(text.ends_with("done_when: the marker block is in the reply\n"));
    assert!(!text.contains("five minutes"));
}
