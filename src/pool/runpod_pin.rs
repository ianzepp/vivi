//! The host key of a pod, pinned at first contact.
//!
//! The provider's API does not carry a pod's SSH host key, so the first
//! contact is the only moment the key can be learned. It is learned twice
//! (`ssh-keyscan`, a pause, `ssh-keyscan`) and used only if both scans show
//! the same non-empty set of keys; the set goes into a known-hosts file of
//! its own for that pod, and every later SSH call checks strictly against
//! that file. A key that differs, a pod that moved to another address, or a
//! missing file fails the call. Nothing is ever accepted on trust a second
//! time, and nothing is learned from a later connection.
//!
//! This guards against a different machine answering later at the same
//! address. It cannot tell a hostile network at the first contact from a good
//! one: the pin is trust on first use, with the two scans as a stability
//! check. It is what the provider's own channel allows.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use super::docker_cli::finish;
use crate::error::VivariumError;

/// Key types a pin may hold.
const KEY_TYPES: [&str; 3] = ["ssh-ed25519", "ecdsa-sha2-nistp256", "ssh-rsa"];
const SCAN_SECS: u64 = 10;
/// Pause between the two scans.
pub const SCAN_GAP: Duration = Duration::from_secs(2);

/// Reads the host keys a server presents. The real one is `ssh-keyscan`.
pub trait KeyScanner {
    /// The raw scan text (`ssh-keyscan` output) for `host:port`.
    ///
    /// # Errors
    /// Returns an error if the scan could not run.
    fn scan(&self, host: &str, port: u16) -> Result<String, VivariumError>;
}

/// `ssh-keyscan` on this machine.
#[derive(Debug, Default)]
pub struct SshKeyscan;

impl KeyScanner for SshKeyscan {
    fn scan(&self, host: &str, port: u16) -> Result<String, VivariumError> {
        check_endpoint(host, port)?;
        let child = Command::new("ssh-keyscan")
            .args([
                "-T",
                &SCAN_SECS.to_string(),
                "-p",
                &port.to_string(),
                "--",
                host,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                VivariumError::Message(format!(
                    "failed to run `ssh-keyscan` (is OpenSSH installed and on PATH?): {e}"
                ))
            })?;
        let out = finish(
            child,
            None,
            Duration::from_secs(SCAN_SECS + 10),
            "ssh-keyscan",
        )?;
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// The address a scan or a known-hosts line may name: a host of letters,
/// digits and `.`/`:`/`-`, and a real port.
fn check_endpoint(host: &str, port: u16) -> Result<(), VivariumError> {
    let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | ':' | '-');
    if host.is_empty() || host.starts_with('-') || !host.chars().all(plain) || port == 0 {
        return Err(VivariumError::Message(format!(
            "the provider gave an SSH address that is not usable: {host}:{port}"
        )));
    }
    Ok(())
}

/// The keys in a scan, as sorted, distinct `<type> <base64>` strings. The
/// order a scan prints them in is not stable, so only the set counts. Lines
/// of other key types, comments and anything malformed are dropped.
#[must_use]
pub fn parse_scan(text: &str) -> Vec<String> {
    let mut keys: Vec<String> = text
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            let [_, kind, blob, ..] = parts.as_slice() else {
                return None;
            };
            let plain = blob
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='));
            (KEY_TYPES.contains(kind) && plain && !blob.is_empty())
                .then(|| format!("{kind} {blob}"))
        })
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

/// The pin from two scans: their common key set.
///
/// # Errors
/// Returns an error if a scan shows no usable key or the two differ.
pub fn pin_from(
    first: &str,
    second: &str,
    host: &str,
    port: u16,
) -> Result<Vec<String>, VivariumError> {
    let (a, b) = (parse_scan(first), parse_scan(second));
    if a.is_empty() || b.is_empty() {
        return Err(VivariumError::Message(format!(
            "{host}:{port} presented no SSH host key; not trusting it"
        )));
    }
    if a != b {
        return Err(VivariumError::Message(format!(
            "the SSH host keys of {host}:{port} changed between two scans; failing closed (nothing is trusted)"
        )));
    }
    Ok(a)
}

/// The known-hosts text for a pin.
#[must_use]
pub fn known_hosts(host: &str, port: u16, keys: &[String]) -> String {
    keys.iter()
        .map(|key| format!("[{host}]:{port} {key}\n"))
        .collect()
}

/// Where the pin of pod `id` lives below `dir`.
#[must_use]
pub fn pin_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.known_hosts"))
}

/// Scan `host:port` twice and write the pin to `path` (private, replaced
/// whole). Returns the keys pinned.
///
/// # Errors
/// Returns an error if a scan fails, shows no key, or the scans differ, or the
/// file cannot be written.
pub fn capture(
    scanner: &dyn KeyScanner,
    (host, port): (&str, u16),
    path: &Path,
    gap: Duration,
) -> Result<Vec<String>, VivariumError> {
    check_endpoint(host, port)?;
    let first = scanner.scan(host, port)?;
    std::thread::sleep(gap);
    let second = scanner.scan(host, port)?;
    let keys = pin_from(&first, &second, host, port)?;
    write_private(path, &known_hosts(host, port, &keys))?;
    Ok(keys)
}

fn write_private(path: &Path, text: &str) -> Result<(), VivariumError> {
    let io =
        |e: std::io::Error| VivariumError::Message(format!("cannot write {}: {e}", path.display()));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    let partial = path.with_extension("partial");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&partial)
        .map_err(io)?;
    file.write_all(text.as_bytes()).map_err(io)?;
    drop(file);
    std::fs::rename(&partial, path).map_err(io)
}

/// Remove the pin of a pod that no longer exists. A missing file is fine.
pub fn forget(path: &Path) {
    std::fs::remove_file(path).map(drop).unwrap_or_default();
}

#[cfg(test)]
#[path = "runpod_pin_test.rs"]
mod tests;
