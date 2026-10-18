//! Tests for the Rust updater: release selection, checksum verification, the
//! versioned install layout, and uninstall. Network paths are exercised through
//! the pure release parser and a local fixture mirror.

use std::fs;
use std::path::{Path, PathBuf};

use semver::Version;

use super::layout::{
    active_version, installed_versions, point_symlink, resolve_bin_dir, run_uninstall,
};
use super::package::{install_release, sha256_hex};
use super::parse_version;
use super::release::{ReleaseJson, consider_release};
use crate::cli::UninstallCommand;

fn release_json(tag: &str, prerelease: bool, draft: bool) -> ReleaseJson {
    serde_json::from_value(serde_json::json!({
        "tag_name": tag,
        "prerelease": prerelease,
        "draft": draft,
        "html_url": format!("https://example.invalid/{tag}"),
    }))
    .unwrap_or_else(|_| ReleaseJson {
        tag_name: tag.into(),
        draft,
        prerelease,
        html_url: String::new(),
    })
}

#[test]
fn release_selection_ignores_prereleases_and_drafts() {
    let releases = vec![
        release_json("v13.4.0", false, false),
        release_json("v14.0.0", false, false),
        release_json("v14.1.0-rc.1", false, false),
        release_json("v14.2.0", true, false),
        release_json("v15.0.0", false, true),
        release_json("notes", false, false),
    ];
    let mut best = None;
    for release in &releases {
        consider_release(&mut best, release);
    }
    let best = best.expect("one stable vivi release");
    assert_eq!(best.version, Version::parse("14.0.0").expect("semver"));
    assert_eq!(best.tag, "v14.0.0");
}

#[test]
fn semver_order_is_numeric() {
    let older = Version::parse("13.9.0").expect("semver");
    let newer = Version::parse("14.0.0").expect("semver");
    assert!(older < newer, "13.9.0 must sort before 14.0.0");
}

#[test]
fn parse_version_strips_optional_v_prefix() {
    assert_eq!(
        parse_version("v14.1.0").expect("semver"),
        Version::parse("14.1.0").expect("semver")
    );
    assert!(parse_version("not-a-version").is_err());
}

fn write_release(mirror: &Path, version: &str, entries: &[(&str, &[u8])], corrupt: bool) {
    let dir = mirror.join(format!("v{version}"));
    fs::create_dir_all(&dir).expect("mirror dir");
    let archive_name = "vivi-test-triple.tar.gz";
    let archive_path = dir.join(archive_name);
    write_tar_gz(&archive_path, entries);
    let digest = sha256_hex(&fs::read(&archive_path).expect("read archive"));
    let digest = if corrupt { "0".repeat(64) } else { digest };
    fs::write(
        dir.join(format!("{archive_name}.sha256")),
        format!("{digest}  {archive_name}\n"),
    )
    .expect("checksum");
}

fn write_tar_gz(path: &Path, entries: &[(&str, &[u8])]) {
    let file = fs::File::create(path).expect("archive file");
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    for (name, data) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, name, *data)
            .expect("archive entry");
    }
    builder
        .into_inner()
        .expect("finish tar")
        .finish()
        .expect("finish gzip");
}

#[test]
fn install_layout_places_a_versioned_binary_and_symlink() {
    let temp = tempfile::tempdir().expect("tmp");
    let mirror = temp.path().join("mirror");
    let bin_dir = temp.path().join("prefix").join("bin");
    write_release(&mirror, "14.1.0", &[("vivi", b"fake vivi")], false);

    install_release(
        mirror.to_str().expect("mirror path"),
        &Version::parse("14.1.0").expect("semver"),
        "test-triple",
        &bin_dir,
    )
    .expect("install");
    let versioned = bin_dir.join("vivi-14.1.0");
    assert_eq!(
        fs::read(&versioned).expect("versioned binary"),
        b"fake vivi"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&versioned)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o755);
        point_symlink(&bin_dir, &Version::parse("14.1.0").expect("semver")).expect("symlink");
        assert_eq!(
            fs::read_link(bin_dir.join("vivi")).expect("link"),
            PathBuf::from("vivi-14.1.0")
        );
        assert_eq!(active_version(&bin_dir).as_deref(), Some("vivi-14.1.0"));
    }
    assert_eq!(
        installed_versions(&bin_dir),
        vec![Version::parse("14.1.0").expect("semver")]
    );
}

#[test]
fn checksum_mismatch_leaves_no_binary() {
    let temp = tempfile::tempdir().expect("tmp");
    let mirror = temp.path().join("mirror");
    let bin_dir = temp.path().join("prefix").join("bin");
    write_release(&mirror, "14.1.0", &[("vivi", b"tampered")], true);

    let error = install_release(
        mirror.to_str().expect("mirror path"),
        &Version::parse("14.1.0").expect("semver"),
        "test-triple",
        &bin_dir,
    )
    .expect_err("checksum mismatch must fail");
    assert!(error.to_string().contains("checksum mismatch"), "{error}");
    assert!(!bin_dir.join("vivi-14.1.0").exists());
    assert!(!bin_dir.join(".vivi-14.1.0.tmp").exists());
}

#[test]
fn archive_without_vivi_is_rejected() {
    let temp = tempfile::tempdir().expect("tmp");
    let mirror = temp.path().join("mirror");
    let bin_dir = temp.path().join("prefix").join("bin");
    write_release(&mirror, "14.1.0", &[("README", b"not a binary")], false);

    let error = install_release(
        mirror.to_str().expect("mirror path"),
        &Version::parse("14.1.0").expect("semver"),
        "test-triple",
        &bin_dir,
    )
    .expect_err("missing vivi must fail");
    assert!(
        error.to_string().contains("does not contain vivi"),
        "{error}"
    );
    assert!(!bin_dir.join("vivi-14.1.0").exists());
}

#[test]
fn resolve_bin_dir_requires_a_managed_install() {
    let temp = tempfile::tempdir().expect("tmp");
    let exe = temp.path().join("vivi");
    fs::write(&exe, b"cargo-installed").expect("exe");
    let error = resolve_bin_dir(None, Some(&exe)).expect_err("unmanaged binary must be refused");
    let text = error.to_string();
    assert!(text.contains("no managed vivi install"), "{text}");
    assert!(text.contains("--prefix"), "{text}");
}

#[cfg(unix)]
#[test]
fn resolve_bin_dir_accepts_a_versioned_install() {
    let temp = tempfile::tempdir().expect("tmp");
    let bin_dir = temp.path();
    fs::write(bin_dir.join("vivi-14.1.0"), b"binary").expect("versioned");
    std::os::unix::fs::symlink("vivi-14.1.0", bin_dir.join("vivi")).expect("link");

    let resolved = resolve_bin_dir(None, Some(&bin_dir.join("vivi-14.1.0"))).expect("managed");
    assert_eq!(resolved, bin_dir);
    let resolved = resolve_bin_dir(None, Some(&bin_dir.join("vivi"))).expect("managed link");
    assert_eq!(resolved, bin_dir);
}

#[test]
fn resolve_bin_dir_accepts_an_explicit_prefix() {
    let temp = tempfile::tempdir().expect("tmp");
    let resolved = resolve_bin_dir(Some(temp.path()), None).expect("explicit prefix");
    assert_eq!(resolved, temp.path().join("bin"));
}

#[cfg(unix)]
#[test]
fn uninstall_one_version_keeps_the_other() {
    let temp = tempfile::tempdir().expect("tmp");
    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&bin_dir).expect("bin dir");
    fs::write(bin_dir.join("vivi-14.0.0"), b"old").expect("14.0.0");
    fs::write(bin_dir.join("vivi-14.1.0"), b"new").expect("14.1.0");
    std::os::unix::fs::symlink("vivi-14.1.0", bin_dir.join("vivi")).expect("link");

    run_uninstall(&UninstallCommand {
        version: Some("14.1.0".to_owned()),
        all: false,
        prefix: Some(temp.path().to_path_buf()),
    })
    .expect("uninstall one");

    assert!(bin_dir.join("vivi-14.0.0").is_file());
    assert!(!bin_dir.join("vivi-14.1.0").exists());
    assert!(fs::symlink_metadata(bin_dir.join("vivi")).is_err());
}

#[cfg(unix)]
#[test]
fn uninstall_all_removes_every_version_and_link() {
    let temp = tempfile::tempdir().expect("tmp");
    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&bin_dir).expect("bin dir");
    fs::write(bin_dir.join("vivi-14.0.0"), b"old").expect("14.0.0");
    fs::write(bin_dir.join("vivi-14.1.0"), b"new").expect("14.1.0");
    std::os::unix::fs::symlink("vivi-14.1.0", bin_dir.join("vivi")).expect("link");

    run_uninstall(&UninstallCommand {
        version: None,
        all: true,
        prefix: Some(temp.path().to_path_buf()),
    })
    .expect("uninstall all");

    assert!(installed_versions(&bin_dir).is_empty());
    assert!(fs::symlink_metadata(bin_dir.join("vivi")).is_err());
}

#[test]
fn install_sh_defaults_to_crate_version() {
    let text = include_str!("../install.sh");
    let version = env!("CARGO_PKG_VERSION");
    assert!(
        text.contains(&format!("VIVI_VERSION:-{version}")),
        "install.sh must default to {version}"
    );
}
