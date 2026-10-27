//! Edge builds: the hub runs each published `vivi/main` commit before it is
//! released, and the mind decides when one goes back.
//!
//! The switch itself belongs to root. A root timer installs the newest edge
//! build and writes a status file the mind reads ([`read`]); after each
//! switch the mind wakes at [`CHECKPOINTS`] and judges the pumps. When it
//! answers `rollback-edge`, [`EdgeActuator`] writes the build's commit into
//! `<mind home>/edge-rollback`; a root path unit reads that one token,
//! checks it is still the active build, and switches back. The mind never
//! runs a privileged command, and a request naming any other build is
//! ignored, so a stale request cannot undo a newer good build.
//!
//! Status file, one `key=value` per line:
//!
//! ```text
//! active=vivi-edge-1a2b3c4d5e6f
//! edge=1a2b3c4d5e6f
//! previous=vivi-22.0.0
//! release=vivi-22.0.0
//! switched_at=1793054583
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use super::actions::{ActionRequest, Actuator};
use super::authority::Action;

/// Where the root installer writes the status file.
pub const DEFAULT_STATUS: &str = "/var/lib/vivi-edge/status";
/// The request file name under the mind home; the root path unit watches it.
pub const REQUEST_FILE: &str = "edge-rollback";
/// Seconds after a switch at which the mind wakes to judge the build.
pub const CHECKPOINTS: [u64; 2] = [5 * 60, 20 * 60];
/// How long after a switch the build stays under watch.
pub const WATCH_SECS: u64 = 45 * 60;
/// Longest status file read; anything longer is not a status file.
const MAX_STATUS: u64 = 4096;

/// Where the mind reads edge status and writes rollback requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeConfig {
    pub status: PathBuf,
    pub request: PathBuf,
}

/// What the root installer last reported.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EdgeStatus {
    /// The binary `/usr/local/bin/vivi` points at.
    pub active: String,
    /// The commit (12 hex) when the active binary is an edge build.
    pub edge: Option<String>,
    pub previous: Option<String>,
    pub release: Option<String>,
    pub switched_at: Option<u64>,
}

/// Parse status text. Unknown keys and malformed lines are skipped; an edge
/// commit that is not 12 lowercase hex is dropped.
#[must_use]
pub fn parse(text: &str) -> EdgeStatus {
    let mut status = EdgeStatus::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "active" => status.active = value.to_string(),
            "edge" => status.edge = Some(value.to_string()).filter(|sha| is_build(sha)),
            "previous" => status.previous = Some(value.to_string()).filter(|v| !v.is_empty()),
            "release" => status.release = Some(value.to_string()).filter(|v| !v.is_empty()),
            "switched_at" => {
                if let Ok(at) = value.parse() {
                    status.switched_at = Some(at);
                }
            }
            _ => {}
        }
    }
    status
}

/// Read the status file. A missing file is `None`: the root installer has not
/// written one, so no edge channel runs on this host yet.
///
/// # Errors
/// Returns the reason when the file exists but cannot be read, is not a
/// regular file, or is longer than a status file can be.
pub fn read(path: &Path) -> Result<Option<EdgeStatus>, String> {
    let meta = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    if !meta.is_file() || meta.len() > MAX_STATUS {
        return Err(format!("{} is not an edge status file", path.display()));
    }
    fs::read_to_string(path)
        .map(|text| Some(parse(&text)))
        .map_err(|error| format!("cannot read {}: {error}", path.display()))
}

/// Whether `value` is an edge build name: a 12-character lowercase hex
/// commit prefix.
#[must_use]
pub fn is_build(value: &str) -> bool {
    value.len() == 12
        && value
            .chars()
            .all(|ch| ch.is_ascii_digit() || ('a'..='f').contains(&ch))
}

impl EdgeStatus {
    /// Whether an edge build is active and still inside its watch window.
    #[must_use]
    pub fn watching(&self, now: u64) -> bool {
        self.edge.is_some()
            && self
                .switched_at
                .is_some_and(|at| now < at.saturating_add(WATCH_SECS))
    }

    /// One wake fingerprint per checkpoint already passed, while the build is
    /// under watch. Each new fingerprint wakes the mind once.
    #[must_use]
    pub fn fingerprints(&self, now: u64) -> Vec<String> {
        let (Some(sha), Some(at)) = (self.edge.as_deref(), self.switched_at) else {
            return Vec::new();
        };
        if !self.watching(now) {
            return Vec::new();
        }
        CHECKPOINTS
            .iter()
            .filter(|offset| now >= at.saturating_add(**offset))
            .map(|offset| format!("edge {sha} +{}m", offset / 60))
            .collect()
    }
}

/// Write a rollback request for `build`. The file is written beside its
/// final name and renamed, so the root unit never reads half a token.
///
/// # Errors
/// Returns the I/O failure text.
pub fn request_rollback(path: &Path, build: &str) -> Result<String, String> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, format!("{build}\n"))
        .and_then(|()| fs::rename(&tmp, path))
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    Ok(format!(
        "rollback of edge build {build} requested; the hub switches back if it is still active"
    ))
}

/// Carries out `rollback-edge` itself and hands every other action to the
/// wrapped actuator.
pub struct EdgeActuator {
    pub inner: Box<dyn Actuator>,
    pub edge: EdgeConfig,
}

impl Actuator for EdgeActuator {
    fn apply(&mut self, request: &ActionRequest) -> Result<String, String> {
        if request.kind != Action::RollbackEdge {
            return self.inner.apply(request);
        }
        let build = request
            .build
            .as_deref()
            .ok_or_else(|| String::from("rollback-edge names no build"))?;
        let status = read(&self.edge.status)?
            .ok_or_else(|| format!("no edge status at {}", self.edge.status.display()))?;
        if status.edge.as_deref() != Some(build) {
            return Err(format!(
                "edge build {build} is not active (active: {})",
                status.active
            ));
        }
        request_rollback(&self.edge.request, build)
    }

    fn feeds_back(&self) -> bool {
        self.inner.feeds_back()
    }
}

#[cfg(test)]
#[path = "edge_test.rs"]
mod tests;
