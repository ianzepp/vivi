//! Bounded, local receipts for pump tick timing shown by `vivi monitor`.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::VivariumError;

const FILE_NAME: &str = "monitor-cycles.jsonl";
const TEMP_NAME: &str = ".monitor-cycles.jsonl.tmp";
const MAX_CYCLES: usize = 64;

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
}

/// A sortable id that stays unique across rapid ticks and process restarts.
#[must_use]
pub fn new_id(sequence: u64) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    format!("{nanos:032x}-{sequence:016x}")
}

/// Read recent cycle receipts without opening `pump.db`.
pub fn read(home: &Path) -> Result<Vec<TickRecord>, VivariumError> {
    let path = home.join(FILE_NAME);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line).map_err(|error| {
                VivariumError::Message(format!(
                    "invalid pump monitor cycle receipt in {}: {error}",
                    path.display()
                ))
            })
        })
        .collect()
}

/// Start a tick receipt, closing a previous orphan as interrupted.
pub fn begin(home: &Path, id: String, started_at: u64) -> Result<(), VivariumError> {
    let mut records = read(home)?;
    for record in records.iter_mut().filter(|record| !record.is_finished()) {
        record.ended_at = Some(started_at);
        record.duration_secs = Some(started_at.saturating_sub(record.started_at));
        record.outcome = Some("interrupted".into());
    }
    records.push(TickRecord {
        id,
        started_at,
        ended_at: None,
        duration_secs: None,
        outcome: None,
    });
    write_recent(home, records)
}

/// Finish a tick receipt with the actual loop outcome and monotonic duration.
pub fn finish(
    home: &Path,
    id: &str,
    ended_at: u64,
    duration_secs: u64,
    outcome: &str,
) -> Result<(), VivariumError> {
    let mut records = read(home)?;
    let Some(record) = records.iter_mut().find(|record| record.id == id) else {
        return Err(VivariumError::Message(format!(
            "pump monitor cycle {id} disappeared before finish"
        )));
    };
    record.ended_at = Some(ended_at);
    record.duration_secs = Some(duration_secs);
    record.outcome = Some(outcome.to_owned());
    write_recent(home, records)
}

fn write_recent(home: &Path, mut records: Vec<TickRecord>) -> Result<(), VivariumError> {
    if records.len() > MAX_CYCLES {
        records.drain(..records.len() - MAX_CYCLES);
    }
    let mut text = String::new();
    for record in records {
        text.push_str(&serde_json::to_string(&record).map_err(|error| {
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
    Ok(())
}

#[cfg(test)]
#[path = "cycle_test.rs"]
mod tests;
