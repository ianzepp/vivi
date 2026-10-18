//! CLI and end-to-end updater tests against a local release mirror.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::Parser;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use vivi::cli::{Cli, Command as CliCommand};

fn current_triple() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        (os, arch) => panic!("no published triple for {arch}-{os}"),
    }
}

fn write_release(mirror: &Path, version: &str, triple: &str, payload: &[u8]) {
    let dir = mirror.join(format!("v{version}"));
    fs::create_dir_all(&dir).unwrap();
    let archive_name = format!("vivi-{triple}.tar.gz");
    let archive_path = dir.join(&archive_name);
    let file = fs::File::create(&archive_path).unwrap();
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    let mut header = tar::Header::new_gnu();
    header.set_size(payload.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    builder.append_data(&mut header, "vivi", payload).unwrap();
    builder.into_inner().unwrap().finish().unwrap();
    let digest = hex::encode(Sha256::digest(fs::read(&archive_path).unwrap()));
    fs::write(
        dir.join(format!("{archive_name}.sha256")),
        format!("{digest}  {archive_name}\n"),
    )
    .unwrap();
}

#[test]
fn parses_update_surface() {
    let check = Cli::try_parse_from(["vivi", "update", "--check"]).unwrap();
    match check.command {
        CliCommand::Update(cmd) => assert!(cmd.check),
        other => panic!("{other:?}"),
    }
    let pin = Cli::try_parse_from([
        "vivi",
        "update",
        "--version",
        "14.0.0",
        "--prefix",
        "/tmp/vivi-prefix",
        "--base-url",
        "/tmp/mirror",
    ])
    .unwrap();
    match pin.command {
        CliCommand::Update(cmd) => {
            assert_eq!(cmd.version.as_deref(), Some("14.0.0"));
            assert_eq!(cmd.prefix.as_deref(), Some(Path::new("/tmp/vivi-prefix")));
            assert_eq!(cmd.base_url.as_deref(), Some("/tmp/mirror"));
        }
        other => panic!("{other:?}"),
    }
    let versions = Cli::try_parse_from(["vivi", "versions", "--prefix", "/tmp/p"]).unwrap();
    match versions.command {
        CliCommand::Versions(cmd) => {
            assert_eq!(cmd.prefix.as_deref(), Some(Path::new("/tmp/p")));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn help_lists_update_commands() {
    let output = Command::new(env!("CARGO_BIN_EXE_vivi"))
        .arg("--help")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("update"), "{text}");
    assert!(text.contains("versions"), "{text}");
    assert!(text.contains("uninstall"), "{text}");
}

#[test]
fn update_help_mentions_check_and_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(["update", "--help"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("--check"), "{text}");
    assert!(text.contains("--version"), "{text}");
    assert!(text.contains("--prefix"), "{text}");
    assert!(text.contains("--base-url"), "{text}");
}

#[test]
fn refuses_unmanaged_binary_without_prefix() {
    let output = Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(["update", "--version", "14.1.0"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains("no managed vivi install"), "{text}");
    assert!(text.contains("--prefix"), "{text}");
}

#[test]
fn update_from_local_mirror_swaps_symlink() {
    let triple = current_triple();
    let temp = TempDir::new().unwrap();
    let mirror = temp.path().join("mirror");
    let prefix = temp.path().join("prefix");
    write_release(&mirror, "14.1.0", triple, b"vivi-14.1.0-bytes");
    write_release(&mirror, "14.0.0", triple, b"vivi-14.0.0-bytes");

    let install = Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args([
            "update",
            "--version",
            "14.1.0",
            "--prefix",
            prefix.to_str().unwrap(),
            "--base-url",
            mirror.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    let bin = prefix.join("bin");
    assert_eq!(
        fs::read(bin.join("vivi-14.1.0")).unwrap(),
        b"vivi-14.1.0-bytes"
    );
    assert_eq!(
        fs::read_link(bin.join("vivi")).unwrap(),
        PathBuf::from("vivi-14.1.0")
    );

    let down = Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args([
            "update",
            "--version",
            "14.0.0",
            "--prefix",
            prefix.to_str().unwrap(),
            "--base-url",
            mirror.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        down.status.success(),
        "{}",
        String::from_utf8_lossy(&down.stderr)
    );
    assert_eq!(
        fs::read_link(bin.join("vivi")).unwrap(),
        PathBuf::from("vivi-14.0.0")
    );
    assert!(bin.join("vivi-14.1.0").is_file());
    assert!(bin.join("vivi-14.0.0").is_file());

    let listed = Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(["versions", "--prefix", prefix.to_str().unwrap()])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&listed.stdout);
    assert!(text.contains("14.1.0"), "{text}");
    assert!(text.contains("14.0.0"), "{text}");
    assert!(text.contains("* 14.0.0"), "{text}");
}
