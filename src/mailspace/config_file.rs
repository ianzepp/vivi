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

use toml_edit::DocumentMut;

use super::MailspaceConfig;
use crate::error::VivariumError;

/// Length of the address prefix: the first characters of the mailspace id.
pub(crate) const PREFIX_LEN: usize = 8;

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
///
/// Used for a newly created file. Later edits go through [`edit_document`]
/// so comments and unused keys stay as the user wrote them.
pub(super) fn write_config(path: &Path, config: &MailspaceConfig) -> Result<(), VivariumError> {
    let raw = toml::to_string_pretty(config)
        .map_err(|e| VivariumError::Other(format!("failed to encode mailspace config: {e}")))?;
    let _lock = lock(path)?;
    replace(path, &raw)
}

/// Parse `path`, run `edit`, and write the document back under the config
/// lock when the text changed. Comments and formatting of untouched keys
/// stay as they were.
pub(crate) fn edit_document(
    path: &Path,
    edit: impl FnOnce(&mut DocumentMut) -> Result<(), VivariumError>,
) -> Result<(), VivariumError> {
    let _lock = lock(path)?;
    let raw = read_raw(path)?;
    let mut doc = parse_document(path, &raw)?;
    edit(&mut doc)?;
    let next = doc.to_string();
    if next != raw {
        replace(path, &next)?;
    }
    Ok(())
}

/// The current file text of `path`.
pub(crate) fn read_raw(path: &Path) -> Result<String, VivariumError> {
    fs::read_to_string(path).map_err(|e| {
        VivariumError::Message(format!(
            "failed to read mailspace config at {}: {e}",
            path.display()
        ))
    })
}

/// Parse TOML that must stay editable (comments kept).
pub(crate) fn parse_document(path: &Path, raw: &str) -> Result<DocumentMut, VivariumError> {
    raw.parse::<DocumentMut>().map_err(|e| {
        VivariumError::Message(format!(
            "failed to parse mailspace config at {}: {e}",
            path.display()
        ))
    })
}

/// Replace `path` with `raw` under the config lock.
pub(crate) fn write_raw(path: &Path, raw: &str) -> Result<(), VivariumError> {
    let _lock = lock(path)?;
    replace(path, raw)
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

/// Make sure `config` carries a valid mailspace id, restoring or minting one
/// when the file has none. An existing id is never overwritten.
///
/// The id is kept twice: as `id` in `mailspace.toml` (the source of truth)
/// and in `mailspace.id` beside it. An older vivi rewrites the config from a
/// struct that has no `id`, dropping the line; it never touches the sidecar,
/// so the next open restores the id instead of minting a different one.
///
/// | config id | sidecar   | action                                     |
/// | --------- | --------- | ------------------------------------------ |
/// | present   | same      | nothing                                    |
/// | present   | absent    | write the sidecar (best effort)            |
/// | present   | different | error naming both; never guess             |
/// | absent    | present   | restore it into the config; never mint     |
/// | absent    | absent    | mint, write both                           |
///
/// Restore and mint re-read the config under the config lock and add the id
/// as the file's first line, so everything else in the file stays as it was.
///
/// # Errors
/// Returns a config error when an id (config or sidecar) is malformed or the
/// two disagree, and an error when a restore or mint cannot be persisted: an
/// unpersisted id is never used. A sidecar that cannot be written next to a
/// config that already has the id is not an error.
pub(super) fn ensure_id(path: &Path, config: &mut MailspaceConfig) -> Result<(), VivariumError> {
    if let Some(id) = config.id.clone() {
        check_id(path, &id)?;
        if sidecar_state(path, &id, true)? == Sidecar::Missing {
            backfill_sidecar(path, &id);
        }
        return Ok(());
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
        if sidecar_state(path, &id, true)? == Sidecar::Missing {
            write_if_missing(path, &id);
        }
        config.id = Some(id);
        return Ok(());
    }
    let id = match read_sidecar(path, false)? {
        Some(id) => {
            eprintln!(
                "vivi: {} had lost its mailspace id; restored {id} from {}",
                path.display(),
                sidecar_path(path).display()
            );
            id
        }
        None => {
            let id = generate_id();
            write_sidecar(path, &id).map_err(|e| persist_error(path, &e))?;
            id
        }
    };
    replace(path, &format!("id = \"{id}\"\n{raw}")).map_err(|e| persist_error(path, &e))?;
    config.id = Some(id);
    Ok(())
}

fn persist_error(path: &Path, error: &VivariumError) -> VivariumError {
    VivariumError::Config(format!(
        "cannot persist the mailspace id for {}: {error}",
        path.display()
    ))
}

/// The id copy beside the config: `<.vivi>/mailspace.id`.
fn sidecar_path(config: &Path) -> PathBuf {
    config.with_file_name("mailspace.id")
}

/// How the sidecar stands against the config's id.
#[derive(Debug, PartialEq, Eq)]
enum Sidecar {
    Same,
    Missing,
}

/// Compare the sidecar with the config's `id`.
///
/// # Errors
/// Returns a config error for a malformed sidecar, or when the two ids
/// differ (naming both ids and both files).
fn sidecar_state(path: &Path, id: &str, config_has_id: bool) -> Result<Sidecar, VivariumError> {
    match read_sidecar(path, config_has_id)? {
        None => Ok(Sidecar::Missing),
        Some(other) if other == id => Ok(Sidecar::Same),
        Some(other) => Err(VivariumError::Config(format!(
            "mailspace id conflict: {} has id {id} but {} has {other}. Keep the id the letters you already sent were minted under (their From address starts with its first 8 characters): to keep the config's id, delete {}; to keep the sidecar's, set the id line in {} to {other}. Nothing was changed.",
            path.display(),
            sidecar_path(path).display(),
            sidecar_path(path).display(),
            path.display(),
        ))),
    }
}

/// The sidecar's id, if the file exists.
///
/// # Errors
/// Returns a config error naming the file when it holds something other than
/// a canonical id, or cannot be read.
fn read_sidecar(path: &Path, config_has_id: bool) -> Result<Option<String>, VivariumError> {
    let sidecar = sidecar_path(path);
    let raw = match fs::read_to_string(&sidecar) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(VivariumError::Config(format!(
                "cannot read the mailspace id copy {}: {e}",
                sidecar.display()
            )));
        }
    };
    let id = raw.trim();
    if is_canonical_id(id) {
        return Ok(Some(id.to_string()));
    }
    let remedy = if config_has_id {
        "delete it to re-derive it from the config"
    } else {
        "delete it to mint a new id"
    };
    Err(VivariumError::Config(format!(
        "{}: '{id}' is not a canonical lowercase hyphenated UUID; {remedy}",
        sidecar.display()
    )))
}

/// Write the sidecar atomically (temp file, then rename). The caller holds
/// the config lock.
fn write_sidecar(path: &Path, id: &str) -> Result<(), VivariumError> {
    let sidecar = sidecar_path(path);
    let mut name = sidecar.as_os_str().to_owned();
    name.push(".tmp");
    let tmp = PathBuf::from(name);
    fs::write(&tmp, format!("{id}\n"))?;
    fs::rename(&tmp, &sidecar)?;
    Ok(())
}

/// Give a config that already has its id the sidecar copy. A safeguard, not
/// a requirement (provisioned and read-only directories): failure is quiet.
/// Returns whether the copy was written.
fn backfill_sidecar(path: &Path, id: &str) -> bool {
    let Ok(_lock) = lock(path) else {
        return false;
    };
    write_if_missing(path, id)
}

/// Write the sidecar unless it exists (another opener may have written it
/// while this one waited for the lock). The caller holds the config lock.
fn write_if_missing(path: &Path, id: &str) -> bool {
    matches!(read_sidecar(path, true), Ok(None)) && write_sidecar(path, id).is_ok()
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
