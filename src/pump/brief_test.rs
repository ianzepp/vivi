//! Tests for [`crate::pump::brief`].

use std::fs;

use tempfile::tempdir;

use crate::pump::brief::{DEFAULT_HAND_RULES, hand_rules};
use crate::pump::config::PumpConfig;
use crate::pump::host::test_ctx;

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
    assert!(DEFAULT_HAND_RULES.contains("Change only the files in WRITE SCOPE"));
    assert!(DEFAULT_HAND_RULES.contains("blocked-scope"));
    assert!(DEFAULT_HAND_RULES.contains("scope_paths"));
    assert!(DEFAULT_HAND_RULES.contains("deviations"));
    assert!(DEFAULT_HAND_RULES.contains("optional = true"));
    assert!(DEFAULT_HAND_RULES.contains("Never answer `built` with a known failing check"));
    assert!(DEFAULT_HAND_RULES.contains("git commit"));
    assert!(DEFAULT_HAND_RULES.contains("cfg-gate"));
    // The old pipeline's words are gone from the rules.
    for gone in ["VERDICT: no-defect", "OUT-OF-SCOPE:", "PROOF"] {
        assert!(
            !DEFAULT_HAND_RULES.contains(gone),
            "default rules still say {gone}"
        );
    }
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
