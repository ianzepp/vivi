//! Tests for the Rust updater: release selection, checksum verification, the
//! versioned install layout, and uninstall. Network paths are exercised through
//! the pure release parser and a local fixture mirror.

use std::fs;
use std::path::{Path, PathBuf};

use semver::Version;

use super::layout::{
    active_version, default_prefix, installed_versions, point_symlink, resolve_bin_dir,
    run_uninstall,
};
use super::package::{install_release, sha256_hex};
use super::parse_version;
use super::release::{ReleaseJson, consider_release};
use super::{bin_dir_on_path, first_shadowing_vivi, header_from_version, path_hint_line};
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
        point_symlink(&bin_dir, &Version::parse("14.1.0").expect("semver"), false)
            .expect("symlink");
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
    let error =
        resolve_bin_dir(None, Some(&exe), false).expect_err("unmanaged binary must be refused");
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

    let resolved =
        resolve_bin_dir(None, Some(&bin_dir.join("vivi-14.1.0")), false).expect("managed");
    assert_eq!(resolved, bin_dir);
    let resolved = resolve_bin_dir(None, Some(&bin_dir.join("vivi")), false).expect("managed link");
    assert_eq!(resolved, bin_dir);
}

#[test]
fn resolve_bin_dir_accepts_an_explicit_prefix() {
    let temp = tempfile::tempdir().expect("tmp");
    let resolved = resolve_bin_dir(Some(temp.path()), None, false).expect("explicit prefix");
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
    assert!(
        text.contains("HOME:?HOME is not set}/.local}"),
        "install.sh must default to ~/.local"
    );
    assert!(
        !text.contains(".local/share/vivi"),
        "install.sh must not default to ~/.local/share/vivi"
    );
    assert!(
        !text.contains("HOME:?HOME is not set}/.vivi\""),
        "install.sh must not default to ~/.vivi"
    );
    assert!(
        text.contains("--adopt"),
        "install.sh must accept --adopt for a plain-file vivi"
    );
}

#[test]
fn resolve_bin_dir_refuses_a_mailspace_prefix() {
    let temp = tempfile::tempdir().expect("tmp");
    let space = temp.path().join(".vivi");
    fs::create_dir_all(space.join("bin")).expect("mailspace bin");
    fs::write(space.join("mailspace.toml"), "name = \"home\"\n").expect("config");
    let error = resolve_bin_dir(Some(&space), None, false).expect_err("mailspace prefix");
    let text = error.to_string();
    assert!(text.contains("mailspace.toml"), "{text}");
    assert!(text.contains(".local"), "{text}");
    assert!(!text.contains(".local/share/vivi"), "{text}");
}

#[cfg(unix)]
#[test]
fn uninstall_refuses_bin_dir_inside_a_mailspace() {
    let temp = tempfile::tempdir().expect("tmp");
    let space = temp.path().join(".vivi");
    let bin_dir = space.join("bin");
    fs::create_dir_all(&bin_dir).expect("bin");
    fs::write(space.join("mailspace.toml"), "name = \"home\"\n").expect("config");
    fs::write(space.join("mail.sqlite"), b"keep-me").expect("sqlite");
    fs::write(bin_dir.join("vivi-14.1.0"), b"binary").expect("versioned");
    std::os::unix::fs::symlink("vivi-14.1.0", bin_dir.join("vivi")).expect("link");

    let error = run_uninstall(&UninstallCommand {
        version: None,
        all: true,
        prefix: Some(space.clone()),
    })
    .expect_err("mailspace uninstall must fail");
    let text = error.to_string();
    assert!(text.contains("mailspace.toml"), "{text}");
    assert!(space.join("mail.sqlite").is_file());
    assert!(space.join("mailspace.toml").is_file());
    assert!(bin_dir.join("vivi-14.1.0").is_file());
    assert!(fs::symlink_metadata(bin_dir.join("vivi")).is_ok());
}

#[cfg(unix)]
#[test]
fn uninstall_leaves_unrelated_files() {
    let temp = tempfile::tempdir().expect("tmp");
    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&bin_dir).expect("bin dir");
    fs::write(bin_dir.join("vivi-14.1.0"), b"binary").expect("versioned");
    fs::write(bin_dir.join("notes.txt"), b"keep").expect("notes");
    fs::write(bin_dir.join("vivi-not-semver"), b"keep").expect("other");
    fs::write(bin_dir.join("vivi.unmanaged"), b"12.0.4").expect("aside");
    std::os::unix::fs::symlink("vivi-14.1.0", bin_dir.join("vivi")).expect("link");

    run_uninstall(&UninstallCommand {
        version: None,
        all: true,
        prefix: Some(temp.path().to_path_buf()),
    })
    .expect("uninstall all");

    assert!(!bin_dir.join("vivi-14.1.0").exists());
    assert!(fs::symlink_metadata(bin_dir.join("vivi")).is_err());
    assert_eq!(fs::read(bin_dir.join("notes.txt")).expect("notes"), b"keep");
    assert_eq!(
        fs::read(bin_dir.join("vivi-not-semver")).expect("other"),
        b"keep"
    );
    assert_eq!(
        fs::read(bin_dir.join("vivi.unmanaged")).expect("aside"),
        b"12.0.4"
    );
}

#[test]
fn resolve_bin_dir_adopt_uses_the_default_prefix() {
    let temp = tempfile::tempdir().expect("tmp");
    let exe = temp.path().join("vivi");
    fs::write(&exe, b"cargo-installed").expect("exe");
    let resolved = resolve_bin_dir(None, Some(&exe), true).expect("adopt default");
    assert_eq!(resolved, default_prefix().join("bin"));
}

#[cfg(unix)]
#[test]
fn point_symlink_refuses_a_plain_vivi_without_adopt() {
    let temp = tempfile::tempdir().expect("tmp");
    let bin_dir = temp.path();
    fs::write(bin_dir.join("vivi-14.1.0"), b"new").expect("versioned");
    fs::write(bin_dir.join("vivi"), b"vivi-12.0.4-plain").expect("plain");

    let error = point_symlink(bin_dir, &Version::parse("14.1.0").expect("semver"), false)
        .expect_err("plain file must be refused");
    let text = error.to_string();
    assert!(text.contains("plain file"), "{text}");
    assert!(text.contains("--adopt"), "{text}");
    assert_eq!(
        fs::read(bin_dir.join("vivi")).expect("plain kept"),
        b"vivi-12.0.4-plain"
    );
    assert!(
        fs::symlink_metadata(bin_dir.join("vivi"))
            .expect("meta")
            .file_type()
            .is_file()
    );
}

#[cfg(unix)]
#[test]
fn point_symlink_adopt_moves_a_plain_vivi_aside() {
    let temp = tempfile::tempdir().expect("tmp");
    let bin_dir = temp.path();
    fs::write(bin_dir.join("vivi-14.1.0"), b"new").expect("versioned");
    fs::write(bin_dir.join("vivi"), b"vivi-12.0.4-plain").expect("plain");
    fs::write(bin_dir.join("other-tool"), b"keep").expect("neighbor");

    point_symlink(bin_dir, &Version::parse("14.1.0").expect("semver"), true).expect("adopt");

    assert_eq!(
        fs::read_link(bin_dir.join("vivi")).expect("link"),
        PathBuf::from("vivi-14.1.0")
    );
    assert_eq!(
        fs::read(bin_dir.join("vivi.unmanaged")).expect("aside"),
        b"vivi-12.0.4-plain"
    );
    assert_eq!(
        fs::read(bin_dir.join("other-tool")).expect("neighbor"),
        b"keep"
    );
    assert!(!bin_dir.join("vivi.unmanaged.1").exists());
}

#[cfg(unix)]
#[test]
fn point_symlink_adopt_numbers_an_existing_aside() {
    let temp = tempfile::tempdir().expect("tmp");
    let bin_dir = temp.path();
    fs::write(bin_dir.join("vivi-14.1.0"), b"new").expect("versioned");
    fs::write(bin_dir.join("vivi"), b"second-plain").expect("plain");
    fs::write(bin_dir.join("vivi.unmanaged"), b"first-plain").expect("aside");

    point_symlink(bin_dir, &Version::parse("14.1.0").expect("semver"), true).expect("adopt");

    assert_eq!(
        fs::read(bin_dir.join("vivi.unmanaged")).expect("first"),
        b"first-plain"
    );
    assert_eq!(
        fs::read(bin_dir.join("vivi.unmanaged.1")).expect("second"),
        b"second-plain"
    );
    assert_eq!(
        fs::read_link(bin_dir.join("vivi")).expect("link"),
        PathBuf::from("vivi-14.1.0")
    );
}

#[cfg(unix)]
#[test]
fn header_from_version_reads_the_active_symlink() {
    let temp = tempfile::tempdir().expect("tmp");
    let prefix = temp.path();
    let bin_dir = prefix.join("bin");
    fs::create_dir_all(&bin_dir).expect("bin");
    fs::write(bin_dir.join("vivi-14.1.0"), b"old").expect("versioned");
    point_symlink(&bin_dir, &Version::parse("14.1.0").expect("semver"), false).expect("link");
    assert_eq!(
        header_from_version(Some(prefix)),
        Version::parse("14.1.0").expect("semver")
    );
}

#[cfg(unix)]
#[test]
fn first_shadowing_vivi_reports_an_earlier_binary() {
    let temp = tempfile::tempdir().expect("tmp");
    let managed = temp.path().join("managed");
    let shadow = temp.path().join("shadow");
    fs::create_dir_all(&managed).expect("managed");
    fs::create_dir_all(&shadow).expect("shadow");
    fs::write(managed.join("vivi"), b"managed").expect("managed vivi");
    fs::write(shadow.join("vivi"), b"old").expect("shadow vivi");
    let shadowed = format!("{}:{}", shadow.display(), managed.display());
    assert_eq!(
        first_shadowing_vivi(&managed, &shadowed).expect("shadow"),
        shadow.join("vivi")
    );
    let managed_first = format!("{}:{}", managed.display(), shadow.display());
    assert!(first_shadowing_vivi(&managed, &managed_first).is_none());
}

#[test]
fn bin_dir_off_path_prints_export_hint() {
    let temp = tempfile::tempdir().expect("tmp");
    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(&bin_dir).expect("bin dir");

    assert!(!bin_dir_on_path(&bin_dir, "/usr/bin:/bin"));
    assert!(!bin_dir_on_path(&bin_dir, ""));
    assert!(!bin_dir_on_path(&bin_dir, "/usr/bin::/bin"));
    let on_path = format!("/usr/bin:{}:/bin", bin_dir.display());
    assert!(bin_dir_on_path(&bin_dir, &on_path));

    let hint = path_hint_line(&bin_dir);
    assert!(hint.contains("export PATH=\""), "{hint}");
    assert!(hint.contains("$PATH"), "{hint}");
    assert!(hint.contains(&bin_dir.display().to_string()), "{hint}");
    assert!(hint.starts_with("add to PATH: "), "{hint}");
}
