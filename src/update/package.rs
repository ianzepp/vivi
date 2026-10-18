//! Download, verify, and extract a release archive.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use semver::Version;
use sha2::{Digest, Sha256};

use super::release;
use crate::VivariumError;

/// Read a release asset from an HTTP(S) URL or a local path.
pub(crate) fn fetch_location(location: &str) -> Result<Vec<u8>, VivariumError> {
    if location.contains("://") {
        return release::http_get(location);
    }
    fs::read(location)
        .map_err(|error| VivariumError::Other(format!("cannot read {location}: {error}")))
}

/// Download, verify, and stage one release binary inside `bin_dir`.
pub(crate) fn install_release(
    base: &str,
    version: &Version,
    triple: &str,
    bin_dir: &Path,
) -> Result<(), VivariumError> {
    let archive_name = format!("vivi-{triple}.tar.gz");
    let archive_url = format!("{base}/v{version}/{archive_name}");
    let archive = fetch_location(&archive_url)?;
    let checksum = fetch_location(&format!("{archive_url}.sha256"))?;
    let expected = parse_checksum(&checksum)?;
    let actual = sha256_hex(&archive);
    if actual != expected {
        return Err(checksum_mismatch(&archive_url, &expected, &actual));
    }
    println!("  checksum: OK");
    write_versioned_bin(bin_dir, version, &archive)
}

fn checksum_mismatch(url: &str, expected: &str, actual: &str) -> VivariumError {
    VivariumError::Other(format!(
        "checksum mismatch for {url}\n  expected: {expected}\n  actual:   {actual}\n  the downloaded archive was not installed"
    ))
}

fn write_versioned_bin(
    bin_dir: &Path,
    version: &Version,
    archive: &[u8],
) -> Result<(), VivariumError> {
    fs::create_dir_all(bin_dir).map_err(|error| {
        VivariumError::Other(format!("cannot create {}: {error}", bin_dir.display()))
    })?;
    let staged = bin_dir.join(format!(".vivi-{version}.tmp"));
    remove_if_present(&staged)?;
    extract_bin(archive, &staged)?;
    rename_staged(&staged, &bin_dir.join(format!("vivi-{version}")))
}

fn rename_staged(staged: &Path, target: &Path) -> Result<(), VivariumError> {
    if let Err(error) = fs::rename(staged, target) {
        remove_if_present(staged)?;
        return Err(VivariumError::Other(format!(
            "cannot install {}: {error}",
            target.display()
        )));
    }
    Ok(())
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

/// Extract `vivi` or `bin/vivi` from the release archive.
pub(crate) fn extract_bin(archive: &[u8], dest: &Path) -> Result<(), VivariumError> {
    let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(archive));
    let mut tar = tar::Archive::new(decoder);
    let entries = tar.entries().map_err(|error| {
        VivariumError::Other(format!("cannot read the release archive: {error}"))
    })?;
    for entry in entries {
        if try_extract_entry(entry, dest)? {
            return Ok(());
        }
    }
    Err(VivariumError::Other(
        "the release archive does not contain vivi".into(),
    ))
}

fn try_extract_entry(
    entry: Result<tar::Entry<flate2::read::GzDecoder<std::io::Cursor<&[u8]>>>, std::io::Error>,
    dest: &Path,
) -> Result<bool, VivariumError> {
    let mut entry = entry.map_err(|error| {
        VivariumError::Other(format!("cannot read the release archive: {error}"))
    })?;
    let path = entry_path(&entry)?;
    if !is_vivi_bin(&path) {
        return Ok(false);
    }
    if !entry.header().entry_type().is_file() {
        return Err(VivariumError::Other(
            "the release archive's vivi is not a regular file".into(),
        ));
    }
    write_extracted(&mut entry, dest)?;
    Ok(true)
}

fn entry_path<R: std::io::Read>(entry: &tar::Entry<R>) -> Result<PathBuf, VivariumError> {
    let path = entry
        .path()
        .map_err(|error| VivariumError::Other(format!("bad path in the release archive: {error}")))?
        .into_owned();
    Ok(path.strip_prefix("./").unwrap_or(&path).to_path_buf())
}

fn is_vivi_bin(path: &Path) -> bool {
    path == Path::new("vivi") || path == Path::new("bin/vivi")
}

fn write_extracted<R: std::io::Read>(
    entry: &mut tar::Entry<R>,
    dest: &Path,
) -> Result<(), VivariumError> {
    let mut file = fs::File::create(dest).map_err(|error| {
        VivariumError::Other(format!("cannot create {}: {error}", dest.display()))
    })?;
    std::io::copy(entry, &mut file)
        .map_err(|error| VivariumError::Other(format!("cannot extract vivi: {error}")))?;
    file.flush().map_err(|error| {
        VivariumError::Other(format!("cannot write {}: {error}", dest.display()))
    })?;
    set_executable(dest)
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), VivariumError> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)
        .map_err(|error| VivariumError::Other(format!("cannot read {}: {error}", path.display())))?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).map_err(|error| {
        VivariumError::Other(format!(
            "cannot make {} executable: {error}",
            path.display()
        ))
    })
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<(), VivariumError> {
    Ok(())
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub(crate) fn parse_checksum(bytes: &[u8]) -> Result<String, VivariumError> {
    let text = std::str::from_utf8(bytes).map_err(|error| {
        VivariumError::Other(format!("the checksum file is not UTF-8: {error}"))
    })?;
    let token = text
        .split_whitespace()
        .next()
        .ok_or_else(|| VivariumError::Other("the checksum file is empty".into()))?;
    if token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(VivariumError::Other(format!(
            "the checksum file does not start with a SHA-256 digest: {token:?}"
        )));
    }
    Ok(token.to_ascii_lowercase())
}
