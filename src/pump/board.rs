//! Need intake: candidate rows, approval marks, planner expiry and launch.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use serde::Serialize;
use serde_json::Value;

use crate::pump::collect::spawn_planner;
use crate::pump::host::Ctx;
use crate::pump::state::State;

/// One row of `vivi need list --json` (pump.py:304-360).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeedRow {
    /// The need handle.
    pub handle: String,
    /// ISO date the need was sent.
    pub date: String,
    /// Subject line.
    pub subject: String,
    /// Marks on the need; the board's approval mark makes it plannable.
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
    let marks = value
        .get("marks")
        .and_then(Value::as_array)
        .map(|marks| {
            marks
                .iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    NeedRow {
        handle: value
            .get("handle")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        date: value
            .get("date")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        subject: value
            .get("subject")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        marks,
    }
}

/// Open needs, from any sender, that carry the approval mark (pump.py:301):
/// held needs first, then needs addressed to the reply identity; duplicate
/// handles keep the first row. An empty mark means no marked rows at all.
#[must_use]
pub fn marked_rows(ctx: &Ctx) -> Vec<NeedRow> {
    let mut found: Vec<NeedRow> = Vec::new();
    let mark = ctx.cfg.board.mark.as_str();
    if mark.is_empty() {
        return found;
    }
    let forms = [
        String::from("need list --status open --json --held"),
        format!("need list --status open --json --to {}", ctx.cfg.reply_from),
    ];
    for args in &forms {
        let out = ctx.vivi(args, 60);
        let Some(rows) = parse_rows(&out.text) else {
            ctx.log(&format!(
                "marked list unreadable ({args}): {}",
                tail_note(&out.text, 150)
            ));
            continue;
        };
        found.extend(
            rows.into_iter()
                .filter(|row| row.marks.iter().any(|m| m.as_str() == mark)),
        );
    }
    let mut unique: Vec<NeedRow> = Vec::new();
    for row in found {
        if !unique.iter().any(|seen| seen.handle == row.handle) {
            unique.push(row);
        }
    }
    unique
}

/// The board's candidate rows (pump.py:325-333): every configured sender's
/// open needs, then marked rows not already present; plus the set of
/// marked (approved) handles.
#[must_use]
pub fn candidate_rows(ctx: &Ctx) -> (Vec<NeedRow>, BTreeSet<String>) {
    let mut rows: Vec<NeedRow> = Vec::new();
    for sender in &ctx.cfg.board.from {
        let out = ctx.vivi(
            &format!("need list --from {sender} --status open --json"),
            60,
        );
        match parse_rows(&out.text) {
            Some(found) => rows.extend(found),
            None => ctx.log(&format!(
                "board list unreadable: {}",
                tail_note(&out.text, 150)
            )),
        }
    }
    let marked = marked_rows(ctx);
    let approved: BTreeSet<String> = marked.iter().map(|row| row.handle.clone()).collect();
    for row in marked {
        if !rows.iter().any(|seen| seen.handle == row.handle) {
            rows.push(row);
        }
    }
    (rows, approved)
}

/// Whitespace-separated handles of `<home>/skip.txt`; missing file means
/// nothing is skipped.
#[must_use]
pub fn read_skip(ctx: &Ctx) -> BTreeSet<String> {
    match fs::read_to_string(ctx.home.join("skip.txt")) {
        Ok(text) => text.split_whitespace().map(String::from).collect(),
        Err(_) => BTreeSet::new(),
    }
}

/// Drop planner entries whose plan file exists (done) or that ran longer
/// than `plan_timeout`; a timed-out planner without a plan file raises
/// `plan-no-answer` (pump.py:338-341).
pub fn expire_planning(ctx: &Ctx, st: &mut State) {
    let timeout = ctx.cfg.board.plan_timeout;
    let mins = timeout / 60;
    let now = ctx.now();
    let expired: Vec<String> = st
        .board
        .planning
        .iter()
        .filter(|(handle, at)| plan_path(ctx, handle).exists() || now - *at > timeout)
        .map(|(handle, _)| handle.clone())
        .collect();
    for handle in expired {
        if !plan_path(ctx, &handle).exists() {
            ctx.exception(
                "plan-no-answer",
                &handle,
                &format!("the planner wrote no plan in {mins} minutes"),
            );
        }
        st.board.planning.remove(&handle);
    }
}

/// Plan the new needs among `rows` (pump.py:343-360): skip old, skipped,
/// known, already-planning or already-planned handles, respect the
/// concurrent-planner cap, then write the need file, spawn the planner and
/// record the planning entry. `replan` bypasses the date cutoff; launched
/// handles leave it and the mirror `st.board.replan` is re-sorted.
pub fn launch_planners(
    ctx: &Ctx,
    st: &mut State,
    rows: &[NeedRow],
    approved: &BTreeSet<String>,
    replan: &mut BTreeSet<String>,
) {
    let skip = read_skip(ctx);
    for row in rows {
        let handle = row.handle.as_str();
        let old = row.date.as_str() < ctx.cfg.board.since.as_str();
        if (old && !replan.contains(handle) && !approved.contains(handle))
            || skip.contains(handle)
            || st.needs.contains_key(handle)
            || st.board.planning.contains_key(handle)
        {
            continue;
        }
        if plan_path(ctx, handle).exists()
            || st.board.planning.len()
                >= usize::try_from(ctx.cfg.board.planners_max).unwrap_or(usize::MAX)
        {
            continue;
        }
        launch_one(ctx, st, row, replan);
    }
}

/// Write the need file, spawn the planner for one row, and record it.
fn launch_one(ctx: &Ctx, st: &mut State, row: &NeedRow, replan: &mut BTreeSet<String>) {
    let handle = row.handle.as_str();
    let thread = ctx.vivi(&format!("need show {handle}"), 60).text;
    let thread: String = thread.chars().take(40_000).collect();
    let needs = ctx.home.join("needs");
    drop(fs::create_dir_all(&needs));
    drop(write_json(
        &needs.join(format!("{handle}.json")),
        &NeedFile {
            need: handle,
            subject: row.subject.as_str(),
            triage: "",
            thread: thread.as_str(),
        },
    ));
    let home = ctx.home.to_string_lossy();
    let prompt = format!(
        "Plan one need, as your system prompt describes. Its thread is in {home}/needs/{handle}.json. \
         Write {home}/plans/{handle}.json. Your final message is one line: handle, verdict, number of units."
    );
    spawn_planner(ctx, "board", handle, &prompt);
    st.board.planning.insert(handle.to_owned(), ctx.now());
    if replan.remove(handle) {
        st.board.replan = replan.iter().cloned().collect();
    }
    let subject: String = row.subject.chars().take(70).collect();
    ctx.log(&format!("board: planning new need {handle} {subject}"));
}

/// `<home>/plans/<handle>.json`.
fn plan_path(ctx: &Ctx, handle: &str) -> PathBuf {
    ctx.home.join("plans").join(format!("{handle}.json"))
}

/// The need file a planner reads.
#[derive(Serialize)]
struct NeedFile<'a> {
    need: &'a str,
    subject: &'a str,
    triage: &'a str,
    thread: &'a str,
}

/// Write `value` as JSON with a one-space indent, like
/// `json.dump(..., indent=1)`.
fn write_json<T: Serialize>(path: &PathBuf, value: &T) -> std::io::Result<()> {
    let file = fs::File::create(path)?;
    let mut writer = std::io::BufWriter::new(file);
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b" ");
    let mut serializer = serde_json::Serializer::with_formatter(&mut writer, formatter);
    value.serialize(&mut serializer)?;
    writer.flush()
}

/// The last `max` characters of `text`, newlines folded to `" | "`.
fn tail_note(text: &str, max: usize) -> String {
    let skip = text.chars().count().saturating_sub(max);
    let tail: String = text.chars().skip(skip).collect();
    tail.replace('\n', " | ")
}
