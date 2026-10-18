//! `vivi pump log`: tail, filter and follow `<home>/log.txt` or `exceptions.txt`.
//!
//! The pure parts ([`LogFilter`], [`tail_lines`]) work on text; [`Follower`]
//! reads appended lines from a file that may be truncated or replaced.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use crate::VivariumError;

use super::control::project_home;

/// How long `--follow` sleeps between polls of the file.
pub const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Which pump file to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFile {
    Log,
    Exceptions,
}

impl LogFile {
    const fn name(self) -> &'static str {
        match self {
            Self::Log => "log.txt",
            Self::Exceptions => "exceptions.txt",
        }
    }
}

/// Line filters; all set filters must match (AND).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogFilter {
    /// Merge-lane id, normalized to `<target>/<n>` form.
    lane: Option<String>,
    unit: Option<String>,
    grep: Option<String>,
}

impl LogFilter {
    /// Build a filter; a lane without `/` is shorthand for `default/<lane>`.
    #[must_use]
    pub fn new(lane: Option<&str>, unit: Option<&str>, grep: Option<&str>) -> Self {
        Self {
            lane: lane.map(normalize_lane),
            unit: unit.map(str::to_owned),
            grep: grep.map(str::to_owned),
        }
    }

    /// True when `line` passes every set filter.
    #[must_use]
    pub fn matches(&self, line: &str) -> bool {
        self.lane
            .as_deref()
            .is_none_or(|lane| has_lane_prefix(line, lane))
            && self
                .unit
                .as_deref()
                .is_none_or(|unit| contains_token(line, unit))
            && self.grep.as_deref().is_none_or(|text| line.contains(text))
    }
}

fn normalize_lane(lane: &str) -> String {
    if lane.contains('/') {
        lane.to_owned()
    } else {
        format!("default/{lane}")
    }
}

/// The text after a leading `HH:MM:SS ` stamp (the whole line if unstamped).
fn after_timestamp(line: &str) -> &str {
    match line.split_once(' ') {
        Some((stamp, rest)) if is_clock(stamp) => rest,
        _ => line,
    }
}

fn is_clock(text: &str) -> bool {
    let parts: Vec<&str> = text.split(':').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_digit()))
}

/// True when the text after the stamp starts with `lane <lane>:` or `lane <lane> `.
fn has_lane_prefix(line: &str, lane: &str) -> bool {
    let Some(rest) = after_timestamp(line).strip_prefix("lane ") else {
        return false;
    };
    rest.strip_prefix(lane)
        .is_some_and(|tail| tail.starts_with(':') || tail.starts_with(' '))
}

fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
}

/// True when `token` occurs in `line` bounded by non-token characters, so
/// `u1` matches `unit u1:` and `u1,` but not `u12` or `pumpfix-u1`.
fn contains_token(line: &str, token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    let bytes = line.as_bytes();
    line.match_indices(token).any(|(start, found)| {
        let before = start.checked_sub(1).map(|i| bytes[i]);
        let after = bytes.get(start + found.len()).copied();
        !before.is_some_and(is_token_byte) && !after.is_some_and(is_token_byte)
    })
}

/// The last `count` lines of `text` that pass `filter` (count applies after
/// filtering). `count == 0` yields nothing.
#[must_use]
pub fn tail_lines<'a>(text: &'a str, filter: &LogFilter, count: usize) -> Vec<&'a str> {
    let mut kept: Vec<&str> = text
        .lines()
        .rev()
        .filter(|line| filter.matches(line))
        .take(count)
        .collect();
    kept.reverse();
    kept
}

/// Reads complete lines appended to a file, surviving truncation and replacement.
#[derive(Debug)]
pub struct Follower {
    path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
}

impl Follower {
    /// Follow `path` from byte `offset`.
    #[must_use]
    pub fn new(path: &Path, offset: u64) -> Self {
        Self {
            path: path.to_owned(),
            offset,
            partial: Vec::new(),
        }
    }

    /// New complete lines since the last poll. A missing file yields nothing;
    /// a file shorter than the read offset is reread from its start.
    pub fn poll(&mut self) -> io::Result<Vec<String>> {
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.reset();
                return Ok(Vec::new());
            }
            Err(error) => return Err(error),
        };
        if file.metadata()?.len() < self.offset {
            self.reset();
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let mut fresh = Vec::new();
        file.read_to_end(&mut fresh)?;
        self.offset += fresh.len() as u64;
        Ok(self.split_lines(fresh))
    }

    fn reset(&mut self) {
        self.offset = 0;
        self.partial.clear();
    }

    fn split_lines(&mut self, fresh: Vec<u8>) -> Vec<String> {
        self.partial.extend(fresh);
        let mut lines = Vec::new();
        while let Some(end) = self.partial.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=end).collect();
            let text = String::from_utf8_lossy(&line[..line.len() - 1]);
            lines.push(text.trim_end_matches('\r').to_owned());
        }
        lines
    }
}

/// Write each line that passes `filter`; false once the reader has gone away.
fn emit<'a>(
    out: &mut impl Write,
    filter: &LogFilter,
    lines: impl IntoIterator<Item = &'a str>,
) -> io::Result<bool> {
    for line in lines.into_iter().filter(|line| filter.matches(line)) {
        match writeln!(out, "{line}") {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => return Ok(false),
            Err(error) => return Err(error),
        }
    }
    match out.flush() {
        Err(error) if error.kind() != io::ErrorKind::BrokenPipe => Err(error),
        result => Ok(result.is_ok()),
    }
}

/// Options for [`show`].
#[derive(Debug, Clone)]
pub struct ShowOptions {
    pub count: usize,
    pub follow: bool,
    pub filter: LogFilter,
}

/// Print the tail of `path`, then (when following) new lines forever, polling
/// every `interval` for at most `max_polls` polls (`None` = unbounded).
/// Returns cleanly when `out` is a closed pipe. A missing file is an error.
pub fn show(
    path: &Path,
    options: &ShowOptions,
    out: &mut impl Write,
    interval: Duration,
    max_polls: Option<usize>,
) -> Result<(), VivariumError> {
    let bytes = fs::read(path).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => {
            VivariumError::Message(format!("no pump log at {}", path.display()))
        }
        _ => VivariumError::Io(error),
    })?;
    let text = String::from_utf8_lossy(&bytes);
    let tail = tail_lines(&text, &options.filter, options.count);
    if !emit(out, &LogFilter::default(), tail)? || !options.follow {
        return Ok(());
    }
    let mut follower = Follower::new(path, bytes.len() as u64);
    let mut polls = 0usize;
    while max_polls.is_none_or(|max| polls < max) {
        thread::sleep(interval);
        polls += 1;
        let lines = follower.poll()?;
        if !emit(out, &options.filter, lines.iter().map(String::as_str))? {
            return Ok(());
        }
    }
    Ok(())
}

/// Resolve the project's pump file and run [`show`] against stdout.
pub fn run(project: &Path, file: LogFile, options: &ShowOptions) -> Result<(), VivariumError> {
    let path = project_home(project)?.join(file.name());
    let stdout = io::stdout();
    show(&path, options, &mut stdout.lock(), POLL_INTERVAL, None)
}
