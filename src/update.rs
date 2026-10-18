//! `vivi update` — resolve, verify, and install a public release binary.
//!
//! The install layout is one file per version plus a symlink:
//!
//! ```text
//! <prefix>/bin/vivi-14.1.0
//! <prefix>/bin/vivi-14.0.0
//! <prefix>/bin/vivi -> vivi-14.1.0
//! ```
//!
//! The versioned file is the receipt. There is no install metadata. A
//! cargo-installed or other unmanaged binary is refused unless `--prefix`
//! is given.

mod layout;
mod package;
mod release;

use semver::Version;

use crate::VivariumError;
use crate::cli::{UninstallCommand, UpdateCommand, VersionsCommand};

/// Outcome of `vivi update --check`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckOutcome {
    /// Newest public release is newer than this binary.
    Available,
    /// This binary matches the newest public release.
    UpToDate,
    /// This binary is newer than the newest public release.
    Ahead,
}

/// Compare `CARGO_PKG_VERSION` to the newest public `v*` release.
///
/// # Errors
///
/// Returns a redacted network or parse error when GitHub cannot be read.
pub fn check_for_update() -> Result<CheckOutcome, VivariumError> {
    let current = current_version()?;
    let newest = release::latest_release()?;
    println!("vivi {current}");
    println!("latest public release: {} ({})", newest.version, newest.tag);
    print_check_detail(&newest);
    Ok(compare_versions(&newest.version, &current))
}

/// Install or activate a release.
///
/// # Errors
///
/// Returns a configuration, network, checksum, or filesystem error.
pub fn run_update(args: &UpdateCommand) -> Result<(), VivariumError> {
    let current = current_version()?;
    let target = target_version(args)?;
    let bin_dir = layout::managed_bin_dir(args.prefix.as_deref())?;
    let triple = current_triple()?;
    println!("vivi update — {current} -> {target} ({triple})");
    println!("  bin:      {}", bin_dir.display());
    stage_binary(args, &target, triple, &bin_dir)?;
    activate(&bin_dir, &target)?;
    Ok(())
}

/// Print installed versions, marking the active symlink target.
///
/// # Errors
///
/// Returns an error when the running binary is not a managed install.
pub fn print_versions(args: &VersionsCommand) -> Result<(), VivariumError> {
    let bin_dir = layout::managed_bin_dir(args.prefix.as_deref())?;
    let active = layout::active_version(&bin_dir);
    let versions = layout::installed_versions(&bin_dir);
    if versions.is_empty() {
        println!("no installed vivi versions in {}", bin_dir.display());
        return Ok(());
    }
    print_version_rows(&versions, active.as_deref());
    println!();
    println!("directory: {}", bin_dir.display());
    Ok(())
}

/// Remove one version, the active version, or every version.
///
/// # Errors
///
/// Returns an error when the prefix is unmanaged or a file cannot be removed.
pub fn uninstall(args: &UninstallCommand) -> Result<(), VivariumError> {
    layout::run_uninstall(args)
}

fn target_version(args: &UpdateCommand) -> Result<Version, VivariumError> {
    match args.version.as_deref() {
        Some(raw) => parse_version(raw),
        None => Ok(release::latest_release()?.version),
    }
}

fn stage_binary(
    args: &UpdateCommand,
    target: &Version,
    triple: &str,
    bin_dir: &std::path::Path,
) -> Result<(), VivariumError> {
    let target_path = bin_dir.join(format!("vivi-{target}"));
    if target_path.is_file() {
        println!("  binary:   {} (already installed)", target_path.display());
        return Ok(());
    }
    let base = asset_base(args);
    println!("  download: {base}/v{target}/vivi-{triple}.tar.gz");
    package::install_release(&base, target, triple, bin_dir)
}

fn activate(bin_dir: &std::path::Path, target: &Version) -> Result<(), VivariumError> {
    let name = format!("vivi-{target}");
    if layout::active_version(bin_dir).as_deref() == Some(name.as_str()) {
        println!("active:     {name} (already active)");
        return Ok(());
    }
    layout::point_symlink(bin_dir, target)?;
    println!("active:     {name}");
    Ok(())
}

fn asset_base(args: &UpdateCommand) -> String {
    args.base_url
        .as_deref()
        .unwrap_or(release::DOWNLOAD_BASE)
        .trim_end_matches('/')
        .to_string()
}

fn print_check_detail(release: &release::PublicRelease) {
    if !release.url.is_empty() {
        println!("  release: {}", release.url);
    }
}

fn compare_versions(latest: &Version, current: &Version) -> CheckOutcome {
    match latest.cmp(current) {
        std::cmp::Ordering::Greater => {
            println!("update available: vivi update");
            CheckOutcome::Available
        }
        std::cmp::Ordering::Equal => {
            println!("up to date");
            CheckOutcome::UpToDate
        }
        std::cmp::Ordering::Less => {
            println!("ahead of the latest public release");
            CheckOutcome::Ahead
        }
    }
}

fn print_version_rows(versions: &[Version], active: Option<&str>) {
    for version in versions {
        let name = format!("vivi-{version}");
        let mark = if active == Some(name.as_str()) {
            "*"
        } else {
            " "
        };
        println!("{mark} {version}");
    }
}

pub(crate) fn current_version() -> Result<Version, VivariumError> {
    parse_version(env!("CARGO_PKG_VERSION"))
}

pub(crate) fn parse_version(raw: &str) -> Result<Version, VivariumError> {
    let trimmed = raw.strip_prefix('v').unwrap_or(raw);
    Version::parse(trimmed)
        .map_err(|error| VivariumError::Other(format!("{raw} is not a semver version: {error}")))
}

/// The triples that publish a prebuilt `vivi` binary.
pub(crate) fn current_triple() -> Result<&'static str, VivariumError> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        (os, arch) => Err(VivariumError::Other(format!(
            "no published vivi binary for {arch}-{os}; releases support \
             aarch64-apple-darwin, x86_64-apple-darwin, and x86_64-unknown-linux-gnu"
        ))),
    }
}

#[cfg(test)]
#[path = "update_test.rs"]
mod update_test;
