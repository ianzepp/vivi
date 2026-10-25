//! CLI surface of the exploratory `vivi mind` spike. Every binary run points
//! the config dir and the mind home at a temp dir, so no real registry,
//! mind state, or socket is touched.

use clap::Parser;
use vivi::cli::{Cli, Command, MindServiceCommand, MindSubcommand};

#[test]
fn bare_mind_parses_as_attach_and_service_verbs_parse() {
    let bare = Cli::try_parse_from(["vivi", "mind"]).unwrap();
    match bare.command {
        Command::Mind(cmd) => assert!(cmd.command.is_none()),
        other => panic!("{other:?}"),
    }
    let run = Cli::try_parse_from(["vivi", "mind", "service", "run"]).unwrap();
    match run.command {
        Command::Mind(cmd) => assert!(matches!(
            cmd.command,
            Some(MindSubcommand::Service {
                command: MindServiceCommand::Run
            })
        )),
        other => panic!("{other:?}"),
    }
    let history = Cli::try_parse_from(["vivi", "mind", "history", "--limit", "5"]).unwrap();
    match history.command {
        Command::Mind(cmd) => assert!(matches!(
            cmd.command,
            Some(MindSubcommand::History { limit: 5 })
        )),
        other => panic!("{other:?}"),
    }
}

fn vivi(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(args)
        .env("VIVI_CONFIG_DIR", dir.join("config"))
        .env("VIVI_MIND_HOME", dir.join("mind"))
        .output()
        .unwrap()
}

#[test]
fn status_reports_a_mind_that_is_not_running() {
    let dir = tempfile::tempdir().unwrap();
    let output = vivi(dir.path(), &["mind", "status", "--json"]);
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["running"], false);
    assert_eq!(value["authority"], "observe");
}

#[test]
fn pause_without_a_running_mind_records_it_in_the_temp_state() {
    let dir = tempfile::tempdir().unwrap();
    let output = vivi(dir.path(), &["mind", "pause"]);
    assert!(output.status.success(), "{output:?}");
    let state = std::fs::read_to_string(dir.path().join("mind").join("state.json")).unwrap();
    assert!(state.contains("\"paused\": true"), "{state}");
}

#[test]
fn unit_prints_the_example_and_install_is_a_stub() {
    let dir = tempfile::tempdir().unwrap();
    let unit = vivi(dir.path(), &["mind", "service", "unit"]);
    assert!(unit.status.success());
    assert!(String::from_utf8_lossy(&unit.stdout).contains("[Service]"));
    let install = vivi(dir.path(), &["mind", "service", "install"]);
    assert!(!install.status.success());
    assert!(String::from_utf8_lossy(&install.stderr).contains("not implemented"));
}

#[test]
fn a_later_authority_stage_is_refused_at_load() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config").join("vivi");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("mind.toml"), "authority = \"self-release\"\n").unwrap();
    let output = vivi(dir.path(), &["mind", "status"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("not implemented"));
}
