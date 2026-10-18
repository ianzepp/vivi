//! Versioned install layout: symlink, listing, uninstall.

use std::fs;
use std::path::{Path, PathBuf};

use semver::Version;

use super::parse_version;
use crate::VivariumError;
use crate::cli::UninstallCommand;

/// Resolve the directory that holds `vivi-<version>` and the `vivi` symlink.
///
/// Without an explicit prefix the running binary must be part of a managed
/// install (a `vivi` symlink beside it). A cargo-installed or ad-hoc binary
/// is refused instead of silently replaced, unless `adopt` is set — then
/// the default prefix (`~/.local`, so `~/.local/bin`) is used. `--adopt`
/// never writes into `~/.cargo/bin`.
pub(crate) fn resolve_bin_dir(
    prefix: Option<&Path>,
    exe: Option<&Path>,
    adopt: bool,
) -> Result<PathBuf, VivariumError> {
    if let Some(prefix) = prefix {
        return checked_bin_dir(prefix);
    }
    if let Some(parent) = symlink_install_dir(exe)? {
        return Ok(parent);
    }
    if adopt {
        return checked_bin_dir(&default_prefix());
    }
    Err(unmanaged_from(exe))
}

pub(crate) fn managed_bin_dir(
    prefix: Option<&Path>,
    adopt: bool,
) -> Result<PathBuf, VivariumError> {
    let exe = match std::env::current_exe() {
        Ok(path) => Some(path),
        Err(_) => None,
    };
    resolve_bin_dir(prefix, exe.as_deref(), adopt)
}

fn checked_bin_dir(prefix: &Path) -> Result<PathBuf, VivariumError> {
    let bin_dir = prefix.join("bin");
    refuse_mailspace(&bin_dir)?;
    Ok(bin_dir)
}

fn symlink_install_dir(exe: Option<&Path>) -> Result<Option<PathBuf>, VivariumError> {
    let Some(exe) = exe else {
        return Ok(None);
    };
    let Some(parent) = exe.parent() else {
        return Ok(None);
    };
    if !is_symlink(&parent.join("vivi")) {
        return Ok(None);
    }
    refuse_mailspace(parent)?;
    Ok(Some(parent.to_path_buf()))
}

fn unmanaged_from(exe: Option<&Path>) -> VivariumError {
    match exe {
        Some(path) => match path.parent() {
            Some(parent) => unmanaged_install(parent, path),
            None => VivariumError::Other("cannot locate the running vivi binary".into()),
        },
        None => VivariumError::Other("cannot locate the running vivi binary".into()),
    }
}

fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
}

fn unmanaged_install(parent: &Path, exe: &Path) -> VivariumError {
    VivariumError::Other(format!(
        "no managed vivi install found at {}\n  cause: the running binary ({}) is not a versioned install\n  next: reinstall with install.sh, pass --prefix DIR, or pass --adopt to install under {}/bin",
        parent.display(),
        exe.display(),
        default_prefix().display()
    ))
}

/// Default install prefix used by `install.sh` (`~/.local`, so `~/.local/bin`).
#[must_use]
pub(crate) fn default_prefix() -> PathBuf {
    match dirs::home_dir() {
        Some(home) => home.join(".local"),
        None => PathBuf::from("/usr/local"),
    }
}

fn refuse_mailspace(bin_dir: &Path) -> Result<(), VivariumError> {
    match mailspace_root(bin_dir) {
        Some(root) => Err(VivariumError::Other(format!(
            "refusing to use {} because it sits inside a Vivi mailspace ({}/mailspace.toml)\n  next: install to {} (never a project .vivi directory)",
            bin_dir.display(),
            root.display(),
            default_prefix().display()
        ))),
        None => Ok(()),
    }
}

fn mailspace_root(bin_dir: &Path) -> Option<PathBuf> {
    for dir in bin_dir.ancestors() {
        if dir.join("mailspace.toml").is_file() {
            return Some(dir.to_path_buf());
        }
    }
    None
}

/// Point `<bin_dir>/vivi` at `vivi-<version>` with an atomic rename.
///
/// A plain-file `vivi` is refused unless `adopt` is set. Adopting
/// `rename`s it to `vivi.unmanaged` (or `vivi.unmanaged.N`); it is never
/// deleted. Other names in a shared bin dir are left untouched.
#[cfg(unix)]
pub(crate) fn point_symlink(
    bin_dir: &Path,
    version: &Version,
    adopt: bool,
) -> Result<(), VivariumError> {
    let link = bin_dir.join("vivi");
    prepare_link_slot(&link, adopt)?;
    write_relative_symlink(bin_dir, version, &link)
}

#[cfg(not(unix))]
pub(crate) fn point_symlink(
    _bin_dir: &Path,
    _version: &Version,
    _adopt: bool,
) -> Result<(), VivariumError> {
    Err(VivariumError::Other(
        "vivi updates require a Unix filesystem (symlinks)".into(),
    ))
}

#[cfg(unix)]
fn prepare_link_slot(link: &Path, adopt: bool) -> Result<(), VivariumError> {
    match fs::symlink_metadata(link) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(VivariumError::Other(format!(
            "cannot inspect {}: {error}",
            link.display()
        ))),
        Ok(meta) if meta.file_type().is_symlink() => Ok(()),
        Ok(meta) if meta.file_type().is_file() => adopt_plain_vivi(link, adopt),
        Ok(_) => Err(VivariumError::Other(format!(
            "refusing to replace {}: it is not a vivi file or symlink",
            link.display()
        ))),
    }
}

#[cfg(unix)]
fn adopt_plain_vivi(link: &Path, adopt: bool) -> Result<(), VivariumError> {
    if !adopt {
        return Err(plain_vivi_error(link));
    }
    let parent = link.parent().ok_or_else(|| {
        VivariumError::Other(format!("cannot derive a directory from {}", link.display()))
    })?;
    let dest = unused_aside_path(parent)?;
    fs::rename(link, &dest).map_err(|error| {
        VivariumError::Other(format!(
            "cannot move {} aside to {}: {error}",
            link.display(),
            dest.display()
        ))
    })?;
    println!("moved {} aside to {}", link.display(), dest.display());
    Ok(())
}

#[cfg(unix)]
fn plain_vivi_error(link: &Path) -> VivariumError {
    VivariumError::Other(format!(
        "refusing to replace {}: it is a plain file, not a vivi symlink\n  next: pass --adopt (or --force) to move it aside to vivi.unmanaged",
        link.display()
    ))
}

#[cfg(unix)]
fn unused_aside_path(bin_dir: &Path) -> Result<PathBuf, VivariumError> {
    let first = bin_dir.join("vivi.unmanaged");
    if !entry_exists(&first) {
        return Ok(first);
    }
    for n in 1..=1000 {
        let next = bin_dir.join(format!("vivi.unmanaged.{n}"));
        if !entry_exists(&next) {
            return Ok(next);
        }
    }
    Err(VivariumError::Other(
        "cannot find a free vivi.unmanaged name".into(),
    ))
}

#[cfg(unix)]
fn entry_exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

#[cfg(unix)]
fn write_relative_symlink(
    bin_dir: &Path,
    version: &Version,
    link: &Path,
) -> Result<(), VivariumError> {
    let staged = bin_dir.join(".vivi.link.tmp");
    remove_if_present(&staged)?;
    std::os::unix::fs::symlink(format!("vivi-{version}"), &staged).map_err(|error| {
        VivariumError::Other(format!("cannot create the vivi symlink: {error}"))
    })?;
    if let Err(error) = fs::rename(&staged, link) {
        remove_if_present(&staged)?;
        return Err(VivariumError::Other(format!(
            "cannot point {} at vivi-{version}: {error}",
            link.display()
        )));
    }
    Ok(())
}

/// The active version's file name, read from the `vivi` symlink.
pub(crate) fn active_version(bin_dir: &Path) -> Option<String> {
    let target = match fs::read_link(bin_dir.join("vivi")) {
        Ok(path) => path,
        Err(_) => return None,
    };
    target.file_name()?.to_str().map(str::to_owned)
}

/// Installed versioned binaries, newest first.
pub(crate) fn installed_versions(bin_dir: &Path) -> Vec<Version> {
    let mut versions = Vec::new();
    let Ok(entries) = fs::read_dir(bin_dir) else {
        return versions;
    };
    for entry in entries.flatten() {
        if let Some(version) = versioned_binary(&entry) {
            versions.push(version);
        }
    }
    versions.sort();
    versions.reverse();
    versions
}

fn versioned_binary(entry: &fs::DirEntry) -> Option<Version> {
    let name = entry.file_name();
    let name = name.to_str()?;
    let raw = name.strip_prefix("vivi-")?;
    let version = match Version::parse(raw) {
        Ok(version) => version,
        Err(_) => return None,
    };
    entry.path().is_file().then_some(version)
}

pub(crate) fn run_uninstall(args: &UninstallCommand) -> Result<(), VivariumError> {
    let bin_dir = managed_bin_dir(args.prefix.as_deref(), false)?;
    let active = active_version(&bin_dir);
    let targets = uninstall_targets(args, &bin_dir, active.as_deref())?;
    if targets.is_empty() {
        println!("no installed vivi versions in {}", bin_dir.display());
        return Ok(());
    }
    let removed_link = remove_targets(&bin_dir, &targets, active.as_deref())?;
    print_remaining(&bin_dir, removed_link);
    Ok(())
}

fn uninstall_targets(
    args: &UninstallCommand,
    bin_dir: &Path,
    active: Option<&str>,
) -> Result<Vec<Version>, VivariumError> {
    if args.all {
        return Ok(installed_versions(bin_dir));
    }
    if let Some(raw) = args.version.as_deref() {
        return Ok(vec![parse_version(raw)?]);
    }
    active
        .and_then(|name| name.strip_prefix("vivi-"))
        .map(parse_version)
        .transpose()?
        .map(|version| vec![version])
        .ok_or_else(|| {
            VivariumError::Other(
                "no active vivi version to remove; pass --version <VERSION> or --all".into(),
            )
        })
}

fn remove_targets(
    bin_dir: &Path,
    targets: &[Version],
    active: Option<&str>,
) -> Result<bool, VivariumError> {
    let link = bin_dir.join("vivi");
    let mut removed_link = false;
    for version in targets {
        let name = format!("vivi-{version}");
        remove_versioned(bin_dir, &name)?;
        if active == Some(name.as_str()) && !removed_link {
            removed_link = remove_link(&link)?;
        }
    }
    Ok(removed_link)
}

fn remove_versioned(bin_dir: &Path, name: &str) -> Result<(), VivariumError> {
    let path = bin_dir.join(name);
    match fs::remove_file(&path) {
        Ok(()) => {
            println!("removed {}", path.display());
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!("missing {}", path.display());
            Ok(())
        }
        Err(error) => Err(VivariumError::Other(format!(
            "cannot remove {}: {error}",
            path.display()
        ))),
    }
}

fn remove_link(link: &Path) -> Result<bool, VivariumError> {
    if !is_symlink(link) {
        if link.exists() {
            return Err(VivariumError::Other(format!(
                "refusing to remove {}: it is not the vivi symlink",
                link.display()
            )));
        }
        return Ok(false);
    }
    match fs::remove_file(link) {
        Ok(()) => {
            println!("removed {}", link.display());
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(VivariumError::Other(format!(
            "cannot remove {}: {error}",
            link.display()
        ))),
    }
}

fn print_remaining(bin_dir: &Path, removed_link: bool) {
    let remaining = installed_versions(bin_dir);
    if remaining.is_empty() {
        println!("no installed vivi versions remain");
        return;
    }
    let names: Vec<String> = remaining.iter().map(ToString::to_string).collect();
    println!("remaining: {}", names.join(", "));
    if removed_link {
        println!(
            "no active version; point {} at one of the remaining versions",
            bin_dir.join("vivi").display()
        );
    }
}

fn remove_if_present(path: &Path) -> Result<(), VivariumError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(VivariumError::Other(format!(
            "cannot remove {}: {error}",
            path.display()
        ))),
    }
}
