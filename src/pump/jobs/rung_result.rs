//! Reading a rung job's result from its pool receipt.

use crate::pump::attempt_record::agent_exit;
use crate::pump::jobs::{JobStatus, PoolJob};

/// Most lines of receipt output a result keeps.
const TAIL_LINES: usize = 60;
/// Most bytes of receipt output a result keeps.
const TAIL_BYTES: usize = 4096;

/// What a finished rung command reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RungResult {
    /// The command exited 0.
    pub pass: bool,
    /// The command's exit code; `None` when the job never reported one.
    pub exit: Option<i32>,
    /// The end of the receipt section followed by the stderr tail section of
    /// the posted reply, capped by lines and bytes. A reply with no sections
    /// is tailed whole.
    pub tail: String,
}

/// Read a rung job's result. A timed-out or lost job, or a receipt with no
/// agent exit, does not pass and carries no exit code.
#[must_use]
pub fn parse(job: &PoolJob) -> RungResult {
    let receipt = job.outcome.as_ref().map_or("", |o| o.receipt.as_str());
    let tail = tail_of(&failure_text(receipt));
    let exit = match job.status {
        JobStatus::Finished => agent_exit(receipt),
        _ => None,
    };
    RungResult {
        pass: exit == Some(0),
        exit,
        tail,
    }
}

/// Section headings `pool::record::collected_text` writes.
const HEADINGS: [&str; 4] = [
    "--- facts",
    "--- receipt",
    "--- stderr tail",
    "--- runner log tail",
];

/// The receipt and stderr sections of a posted reply, joined; the whole text
/// when it has no section headings.
fn failure_text(text: &str) -> String {
    if !text.lines().any(|line| HEADINGS.contains(&line)) {
        return text.to_owned();
    }
    let mut kept: Vec<&str> = Vec::new();
    let mut keeping = false;
    for line in text.lines() {
        if HEADINGS.contains(&line) {
            keeping = line == "--- receipt" || line == "--- stderr tail";
        } else if keeping {
            kept.push(line);
        }
    }
    kept.join("\n")
}

fn tail_of(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let kept = lines[lines.len().saturating_sub(TAIL_LINES)..].join("\n");
    if kept.len() <= TAIL_BYTES {
        return kept;
    }
    let mut start = kept.len() - TAIL_BYTES;
    while !kept.is_char_boundary(start) {
        start += 1;
    }
    kept[start..].to_owned()
}

#[cfg(test)]
#[path = "rung_result_test.rs"]
mod tests;
