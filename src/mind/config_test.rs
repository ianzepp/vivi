//! Tests for [`crate::mind::config`].

use std::path::{Path, PathBuf};

use tempfile::tempdir;

use super::*;

fn paths() -> MindPaths {
    MindPaths::under(
        PathBuf::from("/cfg/vivi/mind.toml"),
        Path::new("/home/mind"),
    )
}

#[test]
fn empty_file_is_observe_with_defaults() {
    let config = parse("", paths()).unwrap();
    assert_eq!(config.authority, AuthorityStage::Observe);
    assert_eq!(config.heartbeat, "30m");
    assert_eq!(config.heartbeat_secs, 1800);
    assert_eq!(config.proxy_url, DEFAULT_PROXY_URL);
    assert_eq!(config.pi_command, "pi");
    assert_eq!(config.budgets, Budgets::default());
    assert_eq!(config.paths.socket, PathBuf::from("/home/mind/mind.sock"));
    assert_eq!(
        config.paths.session_dir,
        PathBuf::from("/home/mind/sessions")
    );
    assert_eq!(config.paths.state, PathBuf::from("/home/mind/state.json"));
}

#[test]
fn full_file_parses() {
    let text = r#"
        authority = "unstick"
        heartbeat = "15m"
        proxy_url = "http://localhost:18180"
        pi_command = "/opt/pi/bin/pi"
        model = "kimi/kimi-k2"
        socket = "/run/vivi/mind.sock"
        session_dir = "/var/lib/vivi/mind/sessions"

        [budgets]
        per_need_actions = 2
        per_need_window = "12h"
        max_wakes_per_hour = 4
    "#;
    let config = parse(text, paths()).unwrap();
    assert_eq!(config.authority, AuthorityStage::Unstick);
    assert_eq!(config.heartbeat_secs, 900);
    assert_eq!(config.model.as_deref(), Some("kimi/kimi-k2"));
    assert_eq!(config.paths.socket, PathBuf::from("/run/vivi/mind.sock"));
    assert_eq!(
        config.paths.session_dir,
        PathBuf::from("/var/lib/vivi/mind/sessions")
    );
    assert_eq!(
        config.budgets,
        Budgets {
            per_need_actions: 2,
            per_need_window_secs: 43_200,
            max_wakes_per_hour: 4,
        }
    );
}

#[test]
fn file_fixes_stage_parses() {
    let config = parse("authority = \"file-fixes\"", paths()).unwrap();
    assert_eq!(config.authority, AuthorityStage::FileFixes);
}

#[test]
fn later_stages_are_refused() {
    for stage in ["upgrade-on-approval", "self-release"] {
        let error = parse(&format!("authority = \"{stage}\""), paths()).unwrap_err();
        assert!(error.to_string().contains("not implemented"), "{error}");
    }
}

#[test]
fn unknown_stage_and_unknown_key_are_refused() {
    assert!(parse("authority = \"root\"", paths()).is_err());
    assert!(parse("authority = \"observe\"\nkeys = \"x\"", paths()).is_err());
    assert!(parse("[budgets]\nper_need_tokens = 5", paths()).is_err());
}

#[test]
fn bad_heartbeat_is_refused() {
    assert!(parse("heartbeat = \"soon\"", paths()).is_err());
    assert!(parse("heartbeat = \"0m\"", paths()).is_err());
}

#[test]
fn proxy_must_be_loopback() {
    for ok in [
        "http://127.0.0.1:18180",
        "http://localhost:18180/v1",
        "http://[::1]:18180",
        "http://127.0.0.2:9",
    ] {
        assert!(require_loopback(ok).is_ok(), "{ok}");
    }
    for bad in [
        "http://0.0.0.0:18180",
        "http://10.0.0.5:18180",
        "https://api.example.com",
        "not a url",
    ] {
        assert!(require_loopback(bad).is_err(), "{bad}");
    }
    assert!(parse("proxy_url = \"https://api.example.com\"", paths()).is_err());
}

#[test]
fn stages_order_by_power() {
    assert!(AuthorityStage::Observe < AuthorityStage::Unstick);
    assert!(AuthorityStage::Unstick < AuthorityStage::FileFixes);
    assert!(AuthorityStage::FileFixes < AuthorityStage::UpgradeOnApproval);
    assert!(AuthorityStage::UpgradeOnApproval < AuthorityStage::SelfRelease);
    assert!(!AuthorityStage::UpgradeOnApproval.implemented());
    assert!(!AuthorityStage::SelfRelease.implemented());
}

#[test]
fn load_at_reads_a_temp_file_and_tolerates_a_missing_one() {
    let dir = tempdir().unwrap();
    let home = dir.path().join("home");
    let missing = load_at(&dir.path().join("absent.toml"), &home).unwrap();
    assert_eq!(missing.authority, AuthorityStage::Observe);
    assert_eq!(missing.paths.home, home);
    let file = dir.path().join("mind.toml");
    std::fs::write(&file, "authority = \"unstick\"\nheartbeat = \"1h\"\n").unwrap();
    let config = load_at(&file, &home).unwrap();
    assert_eq!(config.authority, AuthorityStage::Unstick);
    assert_eq!(config.heartbeat_secs, 3600);
    assert_eq!(config.paths.config, file);
}
