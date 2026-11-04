//! Project-declared boot probes.
//!
//! Boot renders the facts Vivi owns: the board, mail, memos, roles, goals, and
//! handle inventory. Facts Vivi cannot own — git ancestry, lane state, the live
//! seat count of a subagent harness — arrive through probes the project
//! declares in `.vivi/mailspace.toml`.
//!
//! A probe is an executable that prints one JSON document on stdout. Vivi
//! invokes it, reads the documented contract, and renders the result. Vivi
//! never learns what a repository, a lane, or a harness is, so the same boot
//! digest serves every project: one with no probes renders the native sections
//! alone.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

/// A probe declared by the project in `mailspace.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeConfig {
    /// Stable probe name, used in the digest and in skip records.
    pub name: String,
    /// Executable path, resolved relative to the mailspace root.
    pub command: String,
}

/// The JSON contract a probe prints on stdout.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeOutput {
    /// Flat header facts, rendered with the digest preamble.
    #[serde(default)]
    pub facts: Vec<String>,
    /// Named blocks of lines, rendered after the native sections.
    #[serde(default)]
    pub sections: Vec<ProbeSection>,
    /// Per-handle verdicts, merged into the handle inventory.
    #[serde(default)]
    pub verdicts: Vec<ProbeVerdict>,
}

/// One named block of probe-supplied lines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeSection {
    pub title: String,
    #[serde(default)]
    pub lines: Vec<String>,
}

/// One probe-supplied verdict about a handle the native sections already list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeVerdict {
    /// Full handle, or a unique handle prefix of at least four characters.
    pub handle: String,
    /// Closed-vocabulary verdict slug (`stale`, `blocked`, `live`, …).
    pub verdict: String,
    #[serde(default)]
    pub detail: Option<String>,
}

/// Outcome of one probe invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProbeRun {
    pub name: String,
    pub output: Option<ProbeOutput>,
    /// Why the probe contributed nothing. A skipped probe is normal.
    pub skipped: Option<String>,
}

impl ProbeRun {
    fn skipped(name: &str, reason: impl Into<String>) -> Self {
        Self {
            name: name.to_string(),
            output: None,
            skipped: Some(reason.into()),
        }
    }
}

/// Run every declared probe. A probe that is missing, fails, or prints
/// unparseable JSON is recorded as skipped and never fails the boot.
#[must_use]
pub fn run_all(root: &Path, probes: &[ProbeConfig]) -> Vec<ProbeRun> {
    probes.iter().map(|probe| run_one(root, probe)).collect()
}

fn run_one(root: &Path, probe: &ProbeConfig) -> ProbeRun {
    let path = resolve_command(root, &probe.command);
    if !path.is_file() {
        return ProbeRun::skipped(&probe.name, format!("missing {}", path.display()));
    }
    let output = match Command::new(&path).current_dir(root).output() {
        Ok(output) => output,
        Err(error) => return ProbeRun::skipped(&probe.name, format!("exec failed: {error}")),
    };
    if !output.status.success() {
        return ProbeRun::skipped(&probe.name, format!("exit {}", output.status));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    match parse_output(&text) {
        Ok(parsed) => ProbeRun {
            name: probe.name.clone(),
            output: Some(parsed),
            skipped: None,
        },
        Err(reason) => ProbeRun::skipped(&probe.name, reason),
    }
}

/// Parse a probe's stdout. Any JSON object with no recognized field is a valid
/// empty contribution, which keeps a probe that has nothing to say from
/// looking like a failure.
///
/// # Errors
/// Returns a human-readable reason when the text is not a JSON object.
pub fn parse_output(stdout: &str) -> Result<ProbeOutput, String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Ok(ProbeOutput::default());
    }
    serde_json::from_str(trimmed).map_err(|error| format!("unparseable JSON: {error}"))
}

fn resolve_command(root: &Path, command: &str) -> PathBuf {
    let candidate = Path::new(command);
    if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        root.join(candidate)
    }
}
