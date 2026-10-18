//! Rotated JSONL access log. Never writes bodies or token values.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;

use super::config::load;
use super::paths::{self, ensure_private_dir};
use crate::VivariumError;
use crate::cli::ProxyLogCommand;

const MAX_BYTES: u64 = 10 * 1024 * 1024;
const KEEP: u32 = 5;
const FOLLOW_POLL: Duration = Duration::from_millis(400);

/// One finished request.
#[derive(Debug, Clone, Serialize)]
pub struct AccessEvent {
    pub ts: String,
    pub id: String,
    pub provider: String,
    pub client: Option<String>,
    pub path: String,
    pub model: Option<String>,
    pub status: u16,
    pub ttfb_ms: u64,
    pub total_ms: u64,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub upstream_id: Option<String>,
}

/// Append-only JSONL writer with size rotation.
pub struct AccessLog {
    dir: PathBuf,
    file: Mutex<File>,
}

impl AccessLog {
    /// Open `{log_dir}/proxy-access.jsonl`, creating the directory at `0700`.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the log cannot be created.
    pub fn open(dir: &Path) -> Result<Self, VivariumError> {
        ensure_private_dir(dir)?;
        let path = access_path(dir);
        let file = open_append(&path)?;
        Ok(Self {
            dir: dir.to_path_buf(),
            file: Mutex::new(file),
        })
    }

    /// Append one event and rotate when the file exceeds the size cap.
    pub fn write(&self, event: &AccessEvent) {
        let Ok(line) = serde_json::to_string(event) else {
            tracing::warn!("could not encode proxy access event");
            return;
        };
        let Ok(mut file) = self.file.lock() else {
            return;
        };
        if writeln!(file, "{line}").is_err() {
            tracing::warn!("could not write proxy access log");
            return;
        }
        drop(file.flush());
        rotate_if_needed(&self.dir, &mut file);
    }
}

/// Start timing a request.
#[must_use]
pub fn start() -> Instant {
    Instant::now()
}

/// Milliseconds since `started`.
#[must_use]
pub fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// RFC3339 timestamp in UTC.
#[must_use]
pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Print or follow the access log / a child provider log.
///
/// # Errors
///
/// Returns a configuration or I/O error.
pub fn print_log(cmd: &ProxyLogCommand) -> Result<(), VivariumError> {
    let config = load(cmd.config.as_deref())?;
    let path = if let Some(provider) = cmd.provider.as_deref() {
        if !config.providers.contains_key(provider) {
            return Err(VivariumError::Config(format!(
                "unknown provider {provider}"
            )));
        }
        child_stdout_path(&config.log_dir, provider)
    } else {
        access_path(&config.log_dir)
    };
    show_path(&path, cmd.lines, cmd.follow)
}

/// Child stdout path used by the supervisor and `vivi proxy log --provider`.
#[must_use]
pub fn child_stdout_path(log_dir: &Path, provider: &str) -> PathBuf {
    log_dir.join(format!("proxy-{provider}.stdout.log"))
}

/// Child stderr path.
#[must_use]
pub fn child_stderr_path(log_dir: &Path, provider: &str) -> PathBuf {
    log_dir.join(format!("proxy-{provider}.stderr.log"))
}

fn access_path(dir: &Path) -> PathBuf {
    dir.join("proxy-access.jsonl")
}

fn open_append(path: &Path) -> Result<File, VivariumError> {
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = file.metadata()?.permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(path, permissions)?;
    }
    Ok(file)
}

fn rotate_if_needed(dir: &Path, file: &mut File) {
    let Ok(metadata) = file.metadata() else {
        return;
    };
    if metadata.len() < MAX_BYTES {
        return;
    }
    drop(file.flush());
    let current = access_path(dir);
    for index in (1..KEEP).rev() {
        let from = dir.join(format!("proxy-access.jsonl.{index}"));
        let to = dir.join(format!("proxy-access.jsonl.{}", index + 1));
        if from.exists() {
            drop(std::fs::rename(from, to));
        }
    }
    drop(std::fs::rename(&current, dir.join("proxy-access.jsonl.1")));
    if let Ok(new_file) = open_append(&current) {
        *file = new_file;
    }
}

fn show_path(path: &Path, lines: usize, follow: bool) -> Result<(), VivariumError> {
    if path.exists() {
        for line in tail_lines(path, lines)? {
            println!("{line}");
        }
    } else if !follow {
        println!("no log at {}", path.display());
    }
    if follow {
        follow_path(path)?;
    }
    Ok(())
}

fn tail_lines(path: &Path, count: usize) -> Result<Vec<String>, VivariumError> {
    let file = File::open(path)?;
    let all: Vec<String> = BufReader::new(file)
        .lines()
        .collect::<Result<Vec<_>, _>>()?;
    let start = all.len().saturating_sub(count);
    Ok(all[start..].to_vec())
}

fn follow_path(path: &Path) -> Result<(), VivariumError> {
    let mut offset = if path.exists() {
        std::fs::metadata(path)?.len()
    } else {
        0
    };
    loop {
        offset = follow_once(path, offset)?;
        std::thread::sleep(FOLLOW_POLL);
    }
}

fn follow_once(path: &Path, mut offset: u64) -> Result<u64, VivariumError> {
    if !path.exists() {
        return Ok(0);
    }
    let len = std::fs::metadata(path)?.len();
    if len < offset {
        offset = 0;
    }
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut buf = String::new();
    file.read_to_string(&mut buf)?;
    for line in buf.lines() {
        println!("{line}");
    }
    Ok(offset + u64::try_from(buf.len()).unwrap_or(0))
}

/// Age of a file for tests and status.
#[must_use]
pub fn file_age(path: &Path) -> Option<Duration> {
    let Ok(metadata) = std::fs::metadata(path) else {
        return None;
    };
    let Ok(modified) = metadata.modified() else {
        return None;
    };
    #[allow(clippy::manual_ok_err)]
    match SystemTime::now().duration_since(modified) {
        Ok(age) => Some(age),
        Err(_) => None,
    }
}

/// Config directory helper used by status when only the path is needed.
///
/// # Errors
///
/// Returns an error when the platform config directory cannot be found.
pub fn config_display_path(explicit: Option<&Path>) -> Result<PathBuf, VivariumError> {
    paths::resolve_config_path(explicit)
}
