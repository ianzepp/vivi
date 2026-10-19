//! Tests for [`crate::pump::brief`].

use std::fs;

use tempfile::tempdir;

use crate::pump::brief::{DEFAULT_HAND_RULES, brief_for, gate_brief, hand_rules};
use crate::pump::config::{Generated, PumpConfig};
use crate::pump::host::{shell_quote, test_ctx};
use crate::pump::state::{Attempt, Unit};

const UID: &str = "56f1d749-2";

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
    assert!(text.contains(
        "done_when: every PROOF command exits 0 on your checkout and the checks named in \
         the rules above pass, or the reply ends with `VERDICT: no-defect` and evidence"
    ));
    assert!(text.contains("VERDICT: no-defect"));
    assert!(text.contains("FINAL REPORT:"));
    assert!(text.contains("the files you changed; what you verified"));
    assert!(text.contains("what still fails and why; anything the next attempt should know"));
    assert!(!text.contains("EARLIER ATTEMPTS"));
    assert!(!text.contains("NEED THREAD"));
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

/// A [`PumpConfig`] carrying one generated-file declaration.
fn cfg_with_generated() -> PumpConfig {
    let mut cfg = PumpConfig::default();
    cfg.merge.generated.push(Generated {
        paths: vec!["scripta/e2e-coverage-ledger.toml".to_owned()],
        regenerate: "./regen-ledger.sh".to_owned(),
        ..Generated::default()
    });
    cfg
}

/// The WRITE SCOPE section of a brief: from its heading to the PROOF heading.
fn scope_section(text: &str) -> &str {
    let start = text.find("WRITE SCOPE:").unwrap();
    &text[start..text.find("PROOF (each must exit 0):").unwrap()]
}

#[test]
fn a_fix_units_brief_lists_generated_paths_as_writable() {
    let dir = tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), cfg_with_generated());
    let text = brief_for(&ctx, "pumpfix-q1-abc", &sample_unit());
    let scope = scope_section(&text);
    assert!(scope.contains("\n- scripta/e2e-coverage-ledger.toml\n"));
    assert!(scope.contains("\n- src/a.rs\n- src/b.rs\n"));
}

#[test]
fn a_plain_units_brief_does_not_list_generated_paths() {
    let dir = tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), cfg_with_generated());
    let text = brief_for(&ctx, "56f1d749-2", &sample_unit());
    assert!(!scope_section(&text).contains("scripta/e2e-coverage-ledger.toml"));
    assert!(!text.contains("scripta/e2e-coverage-ledger.toml"));
}

#[test]
fn a_repair_units_brief_lists_generated_paths_as_writable() {
    let dir = tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), cfg_with_generated());
    let unit = Unit {
        repairs: Some(String::from("q1")),
        ..sample_unit()
    };
    let text = brief_for(&ctx, "56f1d749-2", &unit);
    let scope = scope_section(&text);
    assert!(scope.contains("\n- scripta/e2e-coverage-ledger.toml\n"));
    assert!(scope.contains("\n- src/a.rs\n- src/b.rs\n"));
}

/// An attempt record.
fn attempt(n: u32, model: Option<&str>, why: &str, detail: String) -> Attempt {
    Attempt {
        attempt: n,
        model: model.map(str::to_owned),
        why: why.to_owned(),
        detail,
        at: 0,
        ..Attempt::default()
    }
}

/// One `need show` message block with a header, `Content:` hash and body.
fn message(index: usize, kind: &str, body: &str) -> String {
    format!(
        "## 2026-10-15T10:{:02}:00+00:00 - {:08x} [{kind}]\nFrom: operator@h\nTo: pump@h\n\
         Content: {}\nSubject: s\n\n{body}\n\n",
        index % 60,
        index,
        "ab".repeat(32),
    )
}

/// A `need show` output: the original need, then `replies` as reply messages.
fn thread_text(replies: &[String]) -> String {
    let mut text =
        String::from("Contents (1 message(s))\n\nsubject line\n\nThread (1 message(s))\n\n");
    text.push_str(&message(0, "need", "ORIGINAL-NEED-BODY"));
    for (i, body) in replies.iter().enumerate() {
        text.push_str(&message(i + 1, "reply", body));
    }
    text
}

/// A unit that serves need `abcd1234`.
fn threaded_unit() -> Unit {
    Unit {
        need: "abcd1234".to_owned(),
        ..sample_unit()
    }
}

#[test]
fn history_lists_every_attempt_in_order_with_branch_and_excerpt() {
    let dir = tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let unit = Unit {
        history: vec![
            attempt(1, Some("model-a"), "proof-failed", "#".repeat(2000)),
            attempt(2, None, "agent-replied", "~".repeat(2000)),
            attempt(3, Some("model-c"), "timeout", "^".repeat(3000)),
        ],
        ..sample_unit()
    };
    let text = brief_for(&ctx, UID, &unit);
    let section = text
        .find("EARLIER ATTEMPTS AT THIS UNIT (3 so far")
        .unwrap();
    let one = text.find("- attempt 1 (model-a) ended: proof-failed; branch unit/56f1d749-2-a1");
    let two =
        text.find("- attempt 2 (unknown model) ended: agent-replied; branch unit/56f1d749-2-a2");
    let three = text.find("- attempt 3 (model-c) ended: timeout; branch unit/56f1d749-2-a3");
    let (one, two, three) = (one.unwrap(), two.unwrap(), three.unwrap());
    assert!(section < one && one < two && two < three);
    assert!(text.contains("git fetch origin <branch>"));
    assert!(text.contains("avoid repeating what already failed"));
    assert!(!text.contains("you start clean"));
    assert!(!text.contains("Its work was discarded"));
    // Older attempts keep 500 bytes of detail, the most recent 1800.
    assert_eq!(text.matches('#').count(), 500);
    assert_eq!(text.matches('~').count(), 500);
    assert_eq!(text.matches('^').count(), 1800);
    // The section sits before the closing lines, after the proof.
    assert!(text.find("PROOF (each must exit 0):").unwrap() < section);
    assert!(section < text.find("done_when:").unwrap());
    assert!(text.contains("VERDICT: no-defect"));
}

#[test]
fn history_excerpt_never_splits_a_multibyte_character() {
    let dir = tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    // 900 two-byte characters, then ASCII: a naive byte cut at 1800 lands
    // mid-character, so the excerpt must move the cut forward.
    let detail = format!("{}!{}", "\u{e9}".repeat(900), "y".repeat(500));
    let unit = Unit {
        history: vec![attempt(1, None, "proof-failed", detail)],
        ..sample_unit()
    };
    let text = brief_for(&ctx, UID, &unit);
    // Byte 1800 from the end is byte 501, inside a two-byte character; the
    // cut moves to 502, keeping 649 of the accented characters.
    let expected = format!("    {}!{}", "\u{e9}".repeat(649), "y".repeat(500));
    assert!(text.contains(&expected));
    assert!(!text.contains(&"\u{e9}".repeat(650)));
}

#[test]
fn history_names_the_targets_configured_remote() {
    let dir = tempdir().unwrap();
    let cfg = PumpConfig {
        remote: "upstream".to_owned(),
        ..PumpConfig::default()
    };
    let (ctx, _host) = test_ctx(dir.path(), cfg);
    let unit = Unit {
        history: vec![attempt(1, None, "proof-failed", String::new())],
        ..sample_unit()
    };
    let text = brief_for(&ctx, UID, &unit);
    assert!(text.contains("git fetch upstream <branch>"));
    assert!(text.contains("- attempt 1 (unknown model) ended: proof-failed; branch unit/"));
}

#[test]
fn no_history_means_no_history_section() {
    let dir = tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let text = brief_for(&ctx, UID, &sample_unit());
    assert!(!text.contains("EARLIER ATTEMPTS"));
    assert!(!text.contains("branch unit/"));
}

#[test]
fn the_thread_section_carries_replies_only_newest_last() {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let replies = ["FIRST-REPLY ruling", "SECOND-REPLY clarification"].map(String::from);
    host.reply("need show abcd1234", 0, &thread_text(&replies));
    let text = brief_for(&ctx, UID, &threaded_unit());
    let head = text.find("NEED THREAD REPLIES").unwrap();
    let first = text.find("FIRST-REPLY ruling").unwrap();
    let second = text.find("SECOND-REPLY clarification").unwrap();
    assert!(head < first && first < second);
    assert!(!text.contains("ORIGINAL-NEED-BODY"));
    assert!(!text.contains(&"ab".repeat(32)));
    assert!(text.find("PROOF (each must exit 0):").unwrap() < head);
    assert!(second < text.find("done_when:").unwrap());
    assert!(
        host.commands()
            .iter()
            .any(|c| c.contains("need show abcd1234 --newest 40") && c.contains("--project"))
    );
}

#[test]
fn a_thread_with_only_the_original_need_adds_no_section() {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    host.reply("need show abcd1234", 0, &thread_text(&[]));
    let text = brief_for(&ctx, UID, &threaded_unit());
    assert!(!text.contains("NEED THREAD"));
    assert!(!text.contains("could not be read"));
}

#[test]
fn a_long_thread_is_capped_keeping_the_newest_replies() {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let replies: Vec<String> = (0..40)
        .map(|i| format!("reply-{i:02} {}", "w".repeat(400)))
        .collect();
    host.reply("need show abcd1234", 0, &thread_text(&replies));
    let text = brief_for(&ctx, UID, &threaded_unit());
    assert!(text.contains("reply-39"));
    assert!(!text.contains("reply-00"));
    assert!(text.contains("[older replies cut: showing the newest"));
    let start = text.find("NEED THREAD REPLIES").unwrap();
    let end = text.find("done_when:").unwrap();
    // Heading, cut notice, and at most 6000 bytes of replies.
    assert!(
        end - start <= 6000 + 400,
        "section is {} bytes",
        end - start
    );
    // The cut lands on a message boundary.
    let marker = text.find("[older replies cut").unwrap();
    let after = &text[marker..];
    assert!(
        after
            .split_once('\n')
            .unwrap()
            .1
            .starts_with("## 2026-10-15T")
    );
}

#[test]
fn a_failed_thread_read_leaves_one_notice_and_the_brief_intact() {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    host.reply("need show abcd1234", 124, "partial\n[timeout after 30s]");
    let text = brief_for(&ctx, UID, &threaded_unit());
    assert_eq!(text.matches("could not be read").count(), 1);
    assert!(!text.contains("partial"));
    assert!(!text.contains("NEED THREAD REPLIES"));
    assert!(text.contains("fix the thing described here"));
    assert!(text.contains("WRITE SCOPE:"));
    assert!(text.contains("PROOF (each must exit 0):"));
    assert!(text.contains("VERDICT: no-defect"));
}

#[test]
fn a_unit_without_a_need_never_reads_a_thread() {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let _ = brief_for(&ctx, UID, &sample_unit());
    assert!(!host.commands().iter().any(|c| c.contains("need show")));
}

#[test]
fn a_large_brief_shrinks_history_and_thread_but_never_the_unit() {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let replies: Vec<String> = (0..20)
        .map(|i| format!("reply-{i:02} {}", "w".repeat(400)))
        .collect();
    host.reply("need show abcd1234", 0, &thread_text(&replies));
    let long_brief = format!("{}END-OF-UNIT-BRIEF", "u".repeat(28_000));
    let unit = Unit {
        brief: long_brief.clone(),
        history: (1..=6)
            .map(|n| attempt(n, None, "proof-failed", "#".repeat(3000)))
            .collect(),
        ..threaded_unit()
    };
    let text = brief_for(&ctx, UID, &unit);
    assert!(text.contains(&long_brief));
    assert!(text.contains("\n- src/a.rs\n- src/b.rs\n"));
    assert!(text.contains("\n- `cargo test --lib a`\n- `cargo test --lib b`\n"));
    assert!(text.contains("FINAL REPORT:"));
    // Every attempt is still listed, but the excerpts are gone or cut down.
    assert_eq!(text.matches("; branch unit/56f1d749-2-a").count(), 6);
    assert!(text.matches('#').count() < 6 * 500);
    assert!(!text.contains("reply-00"));
}

#[test]
fn gate_brief_names_the_gate_command_and_the_marker_block() {
    let dir = tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), cfg_with_gate("make gate"));
    let text = gate_brief(&ctx, "0123456789abcdef0123456789abcdef01234567");
    assert!(text.starts_with(
        "Measurement only. Change nothing in the repository; scratch files \
                          go under /tmp."
    ));
    let project = ctx
        .project
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let log = format!("/tmp/pump-gate-{project}-{}-01234567.log", ctx.target_id);
    assert!(text.contains(&format!(
        "    make gate > {} 2>&1; echo \"rc=$?\"",
        shell_quote(&log)
    )));
    assert!(text.contains("FAILING-BEGIN\n<crate> <test name>\nFAILING-END"));
    assert!(text.contains("BUILD-FAILED"));
    assert!(text.contains("TARGET-DIED <target>"));
    assert!(text.contains("ENV <crate> <test name> ::"));
    assert!(text.ends_with("done_when: the marker block is in the reply\n"));
    assert!(!text.contains("five minutes"));
}
