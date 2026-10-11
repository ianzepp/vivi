//! The mailspace config file: locked, atomic writes and the mailspace id.
//!
//! Several agents share one tree, so every write of `mailspace.toml` goes
//! through one advisory lock (`mailspace.toml.lock`, an operating-system
//! `flock`) and lands by temp file plus rename: a reader never sees a torn
//! file. The mailspace id is minted under the same lock, by re-reading the
//! file first, so two processes opening a fresh mailspace at once end with
//! the same id.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use rand_core::{OsRng, RngCore};

use super::MailspaceConfig;
use crate::error::VivariumError;

/// Length of the address prefix: the first characters of the mailspace id.
pub(super) const PREFIX_LEN: usize = 8;

/// Holds the config lock until dropped (closing the file releases it, also
/// when the process dies).
struct ConfigLock {
    _file: File,
}

fn lock_path(config: &Path) -> PathBuf {
    let mut name = config.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

fn lock(config: &Path) -> Result<ConfigLock, VivariumError> {
    let path = lock_path(config);
    let fail = |e: std::io::Error| {
        VivariumError::Config(format!(
            "cannot lock the mailspace config at {}: {e}",
            path.display()
        ))
    };
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(fail)?;
    file.lock().map_err(fail)?;
    Ok(ConfigLock { _file: file })
}

/// Replace the config with `raw`: write a sibling temp file, then rename it
/// over the config. The caller holds the lock.
fn replace(config: &Path, raw: &str) -> Result<(), VivariumError> {
    let mut name = config.as_os_str().to_owned();
    name.push(".tmp");
    let tmp = PathBuf::from(name);
    fs::write(&tmp, raw)?;
    fs::rename(&tmp, config)?;
    Ok(())
}

/// Write `config` to `path` under the config lock, atomically.
pub(super) fn write_config(path: &Path, config: &MailspaceConfig) -> Result<(), VivariumError> {
    let raw = toml::to_string_pretty(config)
        .map_err(|e| VivariumError::Other(format!("failed to encode mailspace config: {e}")))?;
    let _lock = lock(path)?;
    replace(path, &raw)
}

/// Parse a config file's text, naming the file in the error.
pub(super) fn parse_config(path: &Path, raw: &str) -> Result<MailspaceConfig, VivariumError> {
    toml::from_str(raw).map_err(|e| {
        VivariumError::Message(format!(
            "failed to parse mailspace config at {}: {e}",
            path.display()
        ))
    })
}

/// Make sure `config` carries a valid mailspace id, minting and persisting
/// one when the file has none. An existing id is never overwritten.
///
/// The mint re-reads the file under the config lock and writes only if the
/// id is still absent. The id is added as the file's first line, so
/// everything else in the file stays as it was.
///
/// # Errors
/// Returns a config error when a present id is malformed, and an error when
/// the file cannot be locked, read, or written: an unpersisted id is never
/// used.
pub(super) fn ensure_id(path: &Path, config: &mut MailspaceConfig) -> Result<(), VivariumError> {
    if let Some(id) = &config.id {
        return check_id(path, id);
    }
    let _lock = lock(path)?;
    let raw = fs::read_to_string(path).map_err(|e| {
        VivariumError::Message(format!(
            "failed to read mailspace config at {}: {e}",
            path.display()
        ))
    })?;
    if let Some(id) = parse_config(path, &raw)?.id {
        check_id(path, &id)?;
        config.id = Some(id);
        return Ok(());
    }
    let id = generate_id();
    replace(path, &format!("id = \"{id}\"\n{raw}")).map_err(|e| {
        VivariumError::Config(format!(
            "cannot persist a new mailspace id in {}: {e}",
            path.display()
        ))
    })?;
    config.id = Some(id);
    Ok(())
}

fn check_id(path: &Path, id: &str) -> Result<(), VivariumError> {
    if is_canonical_id(id) {
        return Ok(());
    }
    Err(VivariumError::Config(format!(
        "{}: id '{id}' is not a canonical lowercase hyphenated UUID; remove the line to mint a \
         new id",
        path.display()
    )))
}

/// Whether `id` is 8-4-4-4-12 lowercase hex, hyphenated.
#[must_use]
pub(super) fn is_canonical_id(id: &str) -> bool {
    id.len() == 36
        && id.char_indices().all(|(index, ch)| match index {
            8 | 13 | 18 | 23 => ch == '-',
            _ => ch.is_ascii_digit() || ('a'..='f').contains(&ch),
        })
}

/// A random (version 4) UUID in canonical lowercase hyphenated form.
fn generate_id() -> String {
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = hex::encode(bytes);
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}
