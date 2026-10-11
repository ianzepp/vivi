//! `runpodctl` as a subprocess, one call at a time, and the key it needs.
//!
//! Vivi never reads the API key. A call runs `sh -c` with a small fixed
//! wrapper that reads the key file itself and starts `runpodctl` with the key
//! in its environment (`RUNPOD_API_KEY`, which that CLI prefers to its own
//! saved config). The key is never an argument and never a value Vivi holds,
//! so no error text of Vivi's can contain it; error text from the CLI is
//! still scrubbed of anything shaped like a key. The key file must not be
//! readable by group or others.
//!
//! [`Runner`] is the seam tests replace; its arguments are the `runpodctl`
//! arguments.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use super::docker_cli::{RunOutput, Runner, finish};
use super::runpod_config::PoolRunpod;
use crate::error::VivariumError;

pub const BIN: &str = "runpodctl";

/// The wrapper: read the key file, refuse an empty key, hand it to the CLI
/// through the environment only. `$1` is the key file, the rest the command.
const WRAPPER: &str = r#"k=$(cat -- "$1") || exit 97
[ -n "$k" ] || exit 98
RUNPOD_API_KEY=$k
export RUNPOD_API_KEY
shift
exec "$@""#;
const READ_FAILED: i32 = 97;
const EMPTY_KEY: i32 = 98;

/// How long an ordinary call may take.
pub const CALL_TIMEOUT: Duration = Duration::from_mins(1);
/// What a create may take beyond the wait it asks the CLI for: the API call
/// that makes the pod, before the wait starts.
const CREATE_MARGIN: Duration = Duration::from_secs(90);

/// How long Vivi lets a create run when the pod may take `ready_secs` to
/// answer on SSH: the CLI's own wait, plus the call that makes the pod.
#[must_use]
pub fn create_timeout(ready_secs: u64) -> Duration {
    Duration::from_secs(ready_secs) + CREATE_MARGIN
}

/// The wait the create asks the CLI for (`--wait-timeout`), a Go duration.
#[must_use]
pub fn create_wait(ready_secs: u64) -> String {
    format!("{ready_secs}s")
}

/// The real `runpodctl` with one key file.
#[derive(Debug, Clone)]
pub struct CtlCli {
    key_file: PathBuf,
}

impl CtlCli {
    #[must_use]
    pub fn new(config: &PoolRunpod) -> Self {
        Self {
            key_file: config.key_path(),
        }
    }

    /// The process for `args`: the wrapper, the key file path, the CLI.
    #[must_use]
    pub fn command(&self, args: &[String]) -> Command {
        let mut command = Command::new("sh");
        command
            .args(["-c", WRAPPER, "vivi-runpod"])
            .arg(&self.key_file)
            .arg(BIN)
            .args(args)
            .args(["--output", "json"]);
        command
    }
}

/// `runpodctl` with no key: for the questions it answers without an account
/// (its version).
#[derive(Debug, Clone, Copy)]
pub struct BareCli;

impl Runner for BareCli {
    fn run(
        &self,
        args: &[String],
        _stdin: Option<&[u8]>,
        timeout: Duration,
    ) -> Result<RunOutput, VivariumError> {
        let child = Command::new(BIN)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                VivariumError::Message(format!(
                    "failed to run `{BIN}` (is it installed and on PATH?): {e}"
                ))
            })?;
        finish(child, None, timeout, BIN)
    }
}

impl Runner for CtlCli {
    fn run(
        &self,
        args: &[String],
        stdin: Option<&[u8]>,
        timeout: Duration,
    ) -> Result<RunOutput, VivariumError> {
        check_key_file(&self.key_file)?;
        let child = self
            .command(args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| VivariumError::Message(format!("failed to run `sh`: {e}")))?;
        let label = format!(
            "{BIN} {}",
            args.iter().take(2).cloned().collect::<Vec<_>>().join(" ")
        );
        let mut out = finish(child, stdin, timeout, &label)?;
        out.stderr = explain(out.code, &out.stderr);
        Ok(out)
    }
}

/// The text of a failed call: the wrapper's two exit codes in words, the CLI's
/// own text with anything key-shaped removed.
fn explain(code: i32, stderr: &str) -> String {
    match code {
        READ_FAILED => "cannot read the API key file".to_string(),
        EMPTY_KEY => "the API key file is empty".to_string(),
        127 => format!("{BIN} is not installed or not on PATH"),
        _ => redact(stderr.trim()),
    }
}

/// Refuse a key file that is missing, not a file, or readable by group or
/// others. Reads the file's mode only, never its content.
///
/// # Errors
/// Returns an error naming the file and the fix.
pub fn check_key_file(path: &Path) -> Result<(), VivariumError> {
    let shown = path.display();
    let meta = std::fs::metadata(path).map_err(|e| {
        VivariumError::Message(format!(
            "the RunPod API key file {shown} cannot be read ({}); set `api_key_file` in [pools.<name>.runpod] to a file that holds the key",
            e.kind()
        ))
    })?;
    if !meta.is_file() {
        return Err(VivariumError::Message(format!(
            "the RunPod API key file {shown} is not a regular file"
        )));
    }
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(VivariumError::Message(format!(
            "the RunPod API key file {shown} is readable by group or others (mode {mode:03o}); run: chmod 600 {shown}"
        )));
    }
    Ok(())
}

/// Replace anything shaped like the API key or its carrier with `<redacted>`:
/// a `Bearer` token, a `RUNPOD_API_KEY`, `api_key` or `apikey` assignment, and
/// a word that starts `rpa_` (the key prefix).
#[must_use]
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut hide_next = false;
    for (n, word) in text.split_whitespace().enumerate() {
        if n > 0 {
            out.push(' ');
        }
        out.push_str(&redact_word(word, &mut hide_next));
    }
    out
}

fn redact_word(word: &str, hide_next: &mut bool) -> String {
    let lower = word.to_ascii_lowercase();
    if std::mem::take(hide_next) {
        return "<redacted>".to_string();
    }
    if lower == "bearer" {
        *hide_next = true;
        return word.to_string();
    }
    if let Some((name, value)) = word.split_once(['=', ':'])
        && is_key_name(&name.to_ascii_lowercase())
    {
        if value.is_empty() {
            // `api_key: value`: the value is the next word.
            *hide_next = true;
            return word.to_string();
        }
        return format!("{name}=<redacted>");
    }
    if is_key_name(&lower) {
        *hide_next = true;
        return word.to_string();
    }
    if lower
        .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .starts_with("rpa_")
    {
        return "<redacted>".to_string();
    }
    word.to_string()
}

fn is_key_name(lower: &str) -> bool {
    let name = lower.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_');
    matches!(name, "runpod_api_key" | "api_key" | "apikey" | "api-key")
}

#[cfg(test)]
#[path = "runpod_cli_test.rs"]
mod tests;
