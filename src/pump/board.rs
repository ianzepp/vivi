//! The board pass: `sync pull` when due, then the pending `sync push`.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::mailspace::MAILSPACE_DIR;
use crate::pump::flow::tick::raise_once;
use crate::pump::host::{Ctx, shell_quote};
use crate::pump::state::State;
use crate::sync::quarantine;

/// Exception kind: one synced letter is held under `.vivi/quarantine/`.
pub(crate) const SYNC_LETTER_QUARANTINED: &str = "sync-letter-quarantined";

mod replan;
pub use replan::replan_unblocked;

/// One row of `vivi need list --json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeedRow {
    /// The need handle.
    pub handle: String,
    /// ISO date the need was sent.
    pub date: String,
    /// Subject line.
    pub subject: String,
    /// Marks on the need.
    pub marks: Vec<String>,
}

/// Parse the JSON array of a `need list --json` output, skipping any text
/// before the first `[`. Invalid output gives `None`.
#[must_use]
pub fn parse_rows(out: &str) -> Option<Vec<NeedRow>> {
    let start = out.find('[')?;
    let Ok(values) = serde_json::from_str::<Vec<Value>>(&out[start..]) else {
        return None;
    };
    Some(values.iter().map(row_from).collect())
}

/// Build a row from one JSON object; missing keys are empty.
fn row_from(value: &Value) -> NeedRow {
    let text = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let marks = value
        .get("marks")
        .and_then(Value::as_array)
        .map(|marks| {
            marks
                .iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    NeedRow {
        handle: text("handle"),
        date: text("date"),
        subject: text("subject"),
        marks,
    }
}

/// `<home>/plans/<handle>.json`.
fn plan_path(ctx: &Ctx, handle: &str) -> PathBuf {
    ctx.home.join("plans").join(format!("{handle}.json"))
}

/// Verdicts that wait for an operator ruling before anything moves.
pub const PARKED: [&str; 3] = ["operator", "design", "hold"];

/// The last line of `text` cut to `max` chars, empty when the output is blank.
pub(crate) fn last_line(text: &str, max: usize) -> String {
    text.trim()
        .lines()
        .last()
        .unwrap_or("")
        .chars()
        .take(max)
        .collect()
}

/// One board pass: `sync pull` when configured, then the pending push. The
/// interval starts when the pass completes. A non-zero pull only says a
/// quarantine scan is due; the child's text is not parsed.
pub fn board(ctx: &Ctx, st: &mut State) {
    if ctx.now().saturating_sub(st.board.at) < ctx.cfg.board.every {
        return;
    }
    if ctx.cfg.board.pull {
        pull_and_alarm(ctx);
    }
    if std::mem::take(&mut st.board.sync_pending) {
        spawn_sync_push(ctx);
    }
    st.board.at = ctx.now();
}

/// Run `sync pull`. Scan quarantine only when the child exits non-zero.
fn pull_and_alarm(ctx: &Ctx) {
    let out = ctx.vivi("sync pull", 120);
    ctx.log(&format!(
        "sync pull rc={} {}",
        out.code,
        last_line(&out.text, 150)
    ));
    if out.code != 0 {
        alarm_quarantine(ctx);
    }
}

/// Raise [`SYNC_LETTER_QUARANTINED`] once per receipt stem. The directory is
/// the fact. A missing directory means nothing is held.
fn alarm_quarantine(ctx: &Ctx) {
    for (id, path) in quarantine_receipts(ctx) {
        let text = quarantine_detail(&id, &path);
        raise_once(ctx, SYNC_LETTER_QUARANTINED, &id, &text);
    }
}

/// Receipt stems under `.vivi/quarantine/`, in content-id order. `dropped/`
/// is a subdirectory and is not scanned. A missing directory is empty.
#[must_use]
fn quarantine_receipts(ctx: &Ctx) -> Vec<(String, PathBuf)> {
    let root = quarantine::dir(&ctx.project.join(MAILSPACE_DIR));
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            ctx.log(&format!("quarantine scan failed: {error}"));
            return Vec::new();
        }
    };
    let mut found = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => push_receipt(&mut found, &entry.path()),
            Err(error) => ctx.log(&format!("quarantine scan failed: {error}")),
        }
    }
    found.sort();
    found
}

fn push_receipt(found: &mut Vec<(String, PathBuf)>, path: &Path) {
    if path.is_file()
        && let Some(id) = json_stem(path)
    {
        found.push((id, path.to_path_buf()));
    }
}

/// The content id of a `*.json` receipt, or `None` for anything else.
#[must_use]
fn json_stem(path: &Path) -> Option<String> {
    if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
        return None;
    }
    let stem = path.file_stem().and_then(|stem| stem.to_str())?;
    if stem.is_empty() {
        return None;
    }
    Some(stem.to_owned())
}

/// Exception text: the content id, plus the receipt's reason when it reads.
#[must_use]
fn quarantine_detail(id: &str, path: &Path) -> String {
    match receipt_reason(path) {
        Some(reason) => format!("letter {id} quarantined: {reason}"),
        None => format!("letter {id} quarantined"),
    }
}

/// The receipt's `reason` string, when the file is readable JSON that has one.
#[must_use]
fn receipt_reason(path: &Path) -> Option<String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(_) => return None,
    };
    let value = match serde_json::from_str::<Value>(&raw) {
        Ok(value) => value,
        Err(_) => return None,
    };
    let reason = value.get("reason")?.as_str()?.trim();
    if reason.is_empty() {
        None
    } else {
        Some(reason.to_owned())
    }
}

/// Start the detached drain push that ships the replies just posted.
pub(crate) fn spawn_sync_push(ctx: &Ctx) {
    let project = shell_quote(&ctx.project.to_string_lossy());
    let core = format!(
        "{} sync push --since {} --drain --project {project}",
        ctx.vivi, ctx.cfg.board.push_since
    );
    let cmd = if ctx.shell_prefix.is_empty() {
        core
    } else {
        format!("{}; {core}", ctx.shell_prefix)
    };
    ctx.host
        .spawn(&cmd, None, None, &ctx.home.join("sync-push.log"));
    ctx.log("sync push started (detached, drain)");
}
