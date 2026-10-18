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
            assert!(!cmd.adopt);
        }
        other => panic!("{other:?}"),
    }
    let adopt = Cli::try_parse_from(["vivi", "update", "--adopt"]).unwrap();
    match adopt.command {
        CliCommand::Update(cmd) => assert!(cmd.adopt),
        other => panic!("{other:?}"),
    }
    let force = Cli::try_parse_from(["vivi", "update", "--force"]).unwrap();
    match force.command {
        CliCommand::Update(cmd) => assert!(cmd.adopt),
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
    assert!(text.contains("--adopt"), "{text}");
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
    let install_out = String::from_utf8_lossy(&install.stdout);
    assert!(
        install_out.contains("checksum: OK") || install_out.contains("checksum OK"),
        "{install_out}"
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
    let down_out = String::from_utf8_lossy(&down.stdout);
    assert!(down_out.contains("14.1.0 -> 14.0.0"), "{down_out}");
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

#[cfg(unix)]
#[test]
fn update_refuses_a_plain_vivi_unless_adopted() {
    let triple = current_triple();
    let temp = TempDir::new().unwrap();
    let mirror = temp.path().join("mirror");
    let prefix = temp.path().join("prefix");
    let bin = prefix.join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(bin.join("vivi"), b"vivi-12.0.4-plain").unwrap();
    fs::write(bin.join("other-tool"), b"keep").unwrap();
    write_release(&mirror, "14.1.0", triple, b"vivi-14.1.0-bytes");

    let refused = Command::new(env!("CARGO_BIN_EXE_vivi"))
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
    assert!(!refused.status.success());
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&refused.stdout),
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(text.contains("plain file"), "{text}");
    assert!(text.contains("--adopt"), "{text}");
    assert_eq!(fs::read(bin.join("vivi")).unwrap(), b"vivi-12.0.4-plain");
    assert!(bin.join("vivi").is_file());

    let adopted = Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args([
            "update",
            "--version",
            "14.1.0",
            "--prefix",
            prefix.to_str().unwrap(),
            "--base-url",
            mirror.to_str().unwrap(),
            "--adopt",
        ])
        .output()
        .unwrap();
    assert!(
        adopted.status.success(),
        "{}",
        String::from_utf8_lossy(&adopted.stderr)
    );
    assert_eq!(
        fs::read_link(bin.join("vivi")).unwrap(),
        PathBuf::from("vivi-14.1.0")
    );
    assert_eq!(
        fs::read(bin.join("vivi.unmanaged")).unwrap(),
        b"vivi-12.0.4-plain"
    );
    assert_eq!(fs::read(bin.join("other-tool")).unwrap(), b"keep");
    assert_eq!(
        fs::read(bin.join("vivi-14.1.0")).unwrap(),
        b"vivi-14.1.0-bytes"
    );
}

#[cfg(unix)]
#[test]
fn install_sh_refuses_and_adopts_a_plain_vivi() {
    let triple = current_triple();
    let temp = TempDir::new().unwrap();
    let mirror = temp.path().join("mirror");
    let prefix = temp.path().join("prefix");
    let bin = prefix.join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(bin.join("vivi"), b"vivi-12.0.4-plain").unwrap();
    write_release(
        &mirror,
        "14.1.0",
        triple,
        b"#!/bin/sh\necho 'vivi 14.1.0'\n",
    );
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("install.sh");

    let refused = Command::new("sh")
        .arg(&script)
        .args([
            "--version",
            "14.1.0",
            "--prefix",
            prefix.to_str().unwrap(),
            "--base-url",
            mirror.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&refused.stdout),
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(text.contains("plain file"), "{text}");
    assert_eq!(fs::read(bin.join("vivi")).unwrap(), b"vivi-12.0.4-plain");

    let adopted = Command::new("sh")
        .arg(&script)
        .args([
            "--version",
            "14.1.0",
            "--prefix",
            prefix.to_str().unwrap(),
            "--base-url",
            mirror.to_str().unwrap(),
            "--adopt",
        ])
        .output()
        .unwrap();
    assert!(
        adopted.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&adopted.stdout),
        String::from_utf8_lossy(&adopted.stderr)
    );
    assert_eq!(
        fs::read_link(bin.join("vivi")).unwrap(),
        PathBuf::from("vivi-14.1.0")
    );
    assert_eq!(
        fs::read(bin.join("vivi.unmanaged")).unwrap(),
        b"vivi-12.0.4-plain"
    );
}

#[cfg(unix)]
#[test]
fn install_sh_warns_when_another_vivi_is_earlier_on_path() {
    let triple = current_triple();
    let temp = TempDir::new().unwrap();
    let mirror = temp.path().join("mirror");
    let prefix = temp.path().join("prefix");
    let shadow = temp.path().join("shadow");
    fs::create_dir_all(&shadow).unwrap();
    fs::write(shadow.join("vivi"), "#!/bin/sh\necho shadow-vivi\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(shadow.join("vivi"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    write_release(
        &mirror,
        "14.1.1",
        triple,
        b"#!/bin/sh\necho 'vivi 14.1.1'\n",
    );
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("install.sh");
    let path = format!("{}:{}", shadow.display(), std::env::var("PATH").unwrap());
    let output = Command::new("sh")
        .arg(&script)
        .args([
            "--version",
            "14.1.1",
            "--prefix",
            prefix.to_str().unwrap(),
            "--base-url",
            mirror.to_str().unwrap(),
        ])
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains("earlier on PATH"), "{text}");
}
