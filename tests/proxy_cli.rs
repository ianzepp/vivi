use clap::Parser;
use vivi::cli::{Cli, Command, ProxySubcommand};

#[test]
fn parses_proxy_surface() {
    let run = Cli::try_parse_from(["vivi", "proxy", "run", "--config", "/tmp/proxy.toml"]).unwrap();
    match run.command {
        Command::Proxy {
            command: ProxySubcommand::Run(cmd),
        } => assert_eq!(cmd.config.unwrap().to_string_lossy(), "/tmp/proxy.toml"),
        other => panic!("{other:?}"),
    }
    let log = Cli::try_parse_from(["vivi", "proxy", "log", "-f", "--provider", "cursor"]).unwrap();
    match log.command {
        Command::Proxy {
            command: ProxySubcommand::Log(cmd),
        } => {
            assert!(cmd.follow);
            assert_eq!(cmd.provider.as_deref(), Some("cursor"));
        }
        other => panic!("{other:?}"),
    }
    let check = Cli::try_parse_from(["vivi", "proxy", "check", "kimi"]).unwrap();
    match check.command {
        Command::Proxy {
            command: ProxySubcommand::Check(cmd),
        } => assert_eq!(cmd.provider.as_deref(), Some("kimi")),
        other => panic!("{other:?}"),
    }
}

#[test]
fn help_lists_proxy() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_vivi"))
        .arg("proxy")
        .arg("--help")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("run"));
    assert!(text.contains("install"));
    assert!(text.contains("providers"));
    assert!(text.contains("check"));
}

#[test]
fn run_help_names_platform_config_dir() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(["proxy", "run", "--help"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("vivi/proxy.toml"), "{text}");
    assert!(
        !text.contains("dir `/vivi/proxy.toml`"),
        "missing prefix: {text}"
    );
}
