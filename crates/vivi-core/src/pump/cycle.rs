//! Bounded, local receipts for pump tick timing shown by `vivi monitor`.
//!
//! `monitor-cycles.jsonl` is an append-only log: one line when a tick begins
//! and one when it ends, each a whole [`TickRecord`] keyed by the tick id. A
//! reader merges the lines by id, the last line for an id winning, and keeps
//! the newest [`MAX_CYCLES`] ticks. The pump keeps the file open for the life
//! of its run loop, so a tick costs two small appends, and compacts it back to
//! that window once it holds [`COMPACT_LINES`] lines.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write as _};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use vivi_error::VivariumError;

const FILE_NAME: &str = "monitor-cycles.jsonl";
const TEMP_NAME: &str = ".monitor-cycles.jsonl.tmp";
const MAX_CYCLES: usize = 64;
const COMPACT_LINES: usize = 4 * MAX_CYCLES;

/// One actual pump tick, either active or finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TickRecord {
    pub id: String,
    pub started_at: u64,
    pub ended_at: Option<u64>,
    pub duration_secs: Option<u64>,
    pub outcome: Option<String>,
}

impl TickRecord {
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.ended_at.is_some()
    }

    /// This record closed at `at` with `outcome`.
    fn closed(&self, at: u64, duration_secs: u64, outcome: &str) -> TickRecord {
        TickRecord {
            ended_at: Some(at),
            duration_secs: Some(duration_secs),
            outcome: Some(outcome.to_owned()),
            ..self.clone()
        }
    }
}

/// A sortable id that stays unique across rapid ticks and process restarts.
#[must_use]
pub fn new_id(sequence: u64) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    format!("{nanos:032x}-{sequence:016x}")
}

/// Read recent cycle receipts without opening `pump.db`: the log's lines
/// merged by tick id, oldest first, at most [`MAX_CYCLES`]. A final line cut
/// short by a write in progress is ignored.
///
/// # Errors
/// Returns an error when the file cannot be read or an earlier line is not a
/// receipt.
pub fn read(home: &Path) -> Result<Vec<TickRecord>, VivariumError> {
    let path = home.join(FILE_NAME);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let mut records = Vec::with_capacity(lines.len());
    for (n, line) in lines.iter().enumerate() {
        match serde_json::from_str(line) {
            Ok(record) => records.push(record),
            Err(_) if n + 1 == lines.len() && !text.ends_with('\n') => {}
            Err(error) => {
                return Err(VivariumError::Message(format!(
                    "invalid pump monitor cycle receipt in {}: {error}",
                    path.display()
                )));
            }
        }
    }
    Ok(merge(records))
}

/// Merge receipt lines by id, the last line for an id winning, in the order
/// each tick first appeared; keep the newest [`MAX_CYCLES`].
fn merge(lines: Vec<TickRecord>) -> Vec<TickRecord> {
    let mut at: HashMap<String, usize> = HashMap::new();
    let mut records: Vec<TickRecord> = Vec::new();
    for record in lines {
        if let Some(&index) = at.get(&record.id) {
            records[index] = record;
        } else {
            at.insert(record.id.clone(), records.len());
            records.push(record);
        }
    }
    if records.len() > MAX_CYCLES {
        records.drain(..records.len() - MAX_CYCLES);
    }
    records
}

/// The pump's receipt log, kept open across ticks by the run loop.
#[derive(Default)]
pub struct Receipts {
    file: Option<File>,
    /// Lines in the file, counted to know when to compact.
    lines: usize,
    /// The tick begun last, until it is finished.
    open: Option<TickRecord>,
}

impl Receipts {
    /// Start a tick receipt, closing any tick still open as interrupted:
    /// one this process never finished, or, on first use, one a previous
    /// process left behind.
    ///
    /// # Errors
    /// Returns an error when the log cannot be read, compacted or written.
    pub fn begin(&mut self, home: &Path, id: String, started_at: u64) -> Result<(), VivariumError> {
        if self.file.is_none() {
            self.attach(home, started_at)?;
        }
        if let Some(orphan) = self.open.take() {
            let waited = started_at.saturating_sub(orphan.started_at);
            self.append(home, &orphan.closed(started_at, waited, "interrupted"))?;
        }
        let record = TickRecord {
            id,
            started_at,
            ended_at: None,
            duration_secs: None,
            outcome: None,
        };
        self.append(home, &record)?;
        self.open = Some(record);
        Ok(())
    }

    /// Finish the open tick receipt with the loop outcome and monotonic duration.
    ///
    /// # Errors
    /// Returns an error when `id` is not the open tick or the log cannot be
    /// written.
    pub fn finish(
        &mut self,
        home: &Path,
        id: &str,
        ended_at: u64,
        duration_secs: u64,
        outcome: &str,
    ) -> Result<(), VivariumError> {
        let Some(record) = self.open.take_if(|record| record.id == id) else {
            return Err(VivariumError::Message(format!(
                "pump monitor cycle {id} disappeared before finish"
            )));
        };
        self.append(home, &record.closed(ended_at, duration_secs, outcome))
    }

    /// Open the log for appending. Ticks a previous process left open are
    /// closed as interrupted at `now`, by compacting the merged log.
    fn attach(&mut self, home: &Path, now: u64) -> Result<(), VivariumError> {
        let mut records = read(home)?;
        for record in records.iter_mut().filter(|record| !record.is_finished()) {
            let waited = now.saturating_sub(record.started_at);
            *record = record.closed(now, waited, "interrupted");
        }
        self.rewrite(home, &records)
    }

    /// Append one receipt line, compacting first when the log is full.
    fn append(&mut self, home: &Path, record: &TickRecord) -> Result<(), VivariumError> {
        if self.lines >= COMPACT_LINES {
            let records = read(home)?;
            self.rewrite(home, &records)?;
        }
        let mut line = serde_json::to_string(record).map_err(|error| {
            VivariumError::Message(format!("cannot encode pump monitor cycle: {error}"))
        })?;
        line.push('\n');
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| VivariumError::Message("pump monitor cycle log is not open".into()))?;
        file.write_all(line.as_bytes())?;
        self.lines += 1;
        Ok(())
    }

    /// Replace the log with `records`, atomically, and reopen it for appending.
    fn rewrite(&mut self, home: &Path, records: &[TickRecord]) -> Result<(), VivariumError> {
        let mut text = String::new();
        for record in records {
            text.push_str(&serde_json::to_string(record).map_err(|error| {
                VivariumError::Message(format!("cannot encode pump monitor cycle: {error}"))
            })?);
            text.push('\n');
        }
        let path = home.join(FILE_NAME);
        let temporary = home.join(TEMP_NAME);
        fs::write(&temporary, text)?;
        if let Err(error) = fs::rename(&temporary, &path) {
            let _ = fs::remove_file(&temporary);
            return Err(error.into());
        }
        self.file = Some(OpenOptions::new().append(true).open(&path)?);
        self.lines = records.len();
        Ok(())
    }
}

#[cfg(test)]
#[path = "cycle_test.rs"]
mod tests;
