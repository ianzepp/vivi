//! The board pass: `sync pull` when due, then the pending `sync push`.

use std::path::PathBuf;

use serde_json::Value;

use crate::pump::host::{Ctx, shell_quote};
use crate::pump::state::{State, Status, Unit};

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

pub(crate) fn unfinished_closed_need_unit(unit: &Unit) -> bool {
    matches!(
        unit.status,
        Status::Pending
            | Status::Held
            | Status::NeedsReplan
            | Status::Inflight
            | Status::Delivered
            | Status::Merging
    )
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
/// interval starts when the pass completes.
pub fn board(ctx: &Ctx, st: &mut State) {
    if ctx.now().saturating_sub(st.board.at) < ctx.cfg.board.every {
        return;
    }
    if ctx.cfg.board.pull {
        let out = ctx.vivi("sync pull", 120);
        ctx.log(&format!(
            "sync pull rc={} {}",
            out.code,
            last_line(&out.text, 150)
        ));
    }
    if std::mem::take(&mut st.board.sync_pending) {
        spawn_sync_push(ctx);
    }
    st.board.at = ctx.now();
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
