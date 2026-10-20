//! Cadence-driven Mind oversight: a compact pump brief, an allowlisted set
//! of steering actions, recorded interventions, and no tick blockage.

use std::collections::BTreeSet;
use std::fs;

use serde::{Deserialize, Serialize};

use crate::mailspace::RoleView;
use crate::pump::host::Ctx;
use crate::pump::jobs::{self, JobOutcome};
use crate::pump::state::State;

/// `state.extra` key for recorded interventions.
pub const EXTRA_KEY: &str = "oversight";

/// Receipt marker the Mind must emit.
pub const RESULT_MARKER: &str = "PUMP_OVERSIGHT_RESULT=";

const ALLOWED: [&str; 6] = [
    "hold-merge",
    "clear-hold-merge",
    "skip-unit",
    "unskip-unit",
    "replan-need",
    "note",
];

/// One recorded steering attempt, applied or refused.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Intervention {
    pub at: u64,
    pub job: String,
    pub op: String,
    pub target: String,
    pub result: String,
}

/// Durable list of Mind interventions.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct OversightState {
    #[serde(default)]
    pub interventions: Vec<Intervention>,
}

#[derive(Deserialize)]
struct OversightResult {
    #[serde(default)]
    actions: Vec<Action>,
}

#[derive(Deserialize, Default)]
struct Action {
    #[serde(default)]
    op: String,
    #[serde(default)]
    id: String,
    #[serde(default)]
    text: String,
}

/// Cadence job parts for a Mind role: read-only, with a compact pump brief.
#[must_use]
pub fn job_parts(ctx: &Ctx, st: &State, role: &RoleView, now: u64) -> (String, String, String) {
    let subject = format!("oversight {} {}", role.name, now);
    let body = format!(
        "You are the Mind on a cadence oversight wake. Do not implement product work.\n\
         Steer only through the allowlisted actions. Unknown ops are refused and recorded;\n\
         they never block the pump tick.\n\
         Allowlist: hold-merge, clear-hold-merge, skip-unit, unskip-unit, replan-need, note.\n\
         End with one minified JSON line prefixed exactly `{RESULT_MARKER}`.\n\
         Shape: {{\"version\":1,\"actions\":[{{\"op\":\"<name>\",\"id\":\"<unit-or-need>\",\"text\":\"<note>\"}}]}}.\n\
         `id` is required for skip-unit, unskip-unit, and replan-need. `text` is the note body.\n\n\
         PUMP-STATE-BEGIN\n{}\nPUMP-STATE-END\n",
        compact_state(ctx, st)
    );
    (String::from("cadence"), subject, body)
}

/// Apply allowlisted actions from a finished oversight job. Parse and I/O
/// failures are logged; this function never returns an error to the tick.
pub fn apply(ctx: &Ctx, st: &mut State, job_id: &str) {
    let Some(job) = st.jobs.get(job_id) else {
        return;
    };
    if job.payload.get("oversight") != Some(&serde_json::Value::Bool(true)) {
        return;
    }
    let receipt = job
        .outcome
        .as_ref()
        .map_or("", |outcome: &JobOutcome| outcome.receipt.as_str());
    let Some(result) = parse_result(receipt) else {
        ctx.log(&format!("oversight {job_id}: no result marker; skipped"));
        return;
    };
    let actions = result.actions;
    for action in actions {
        apply_one(ctx, st, job_id, action);
    }
}

fn compact_state(ctx: &Ctx, st: &State) -> String {
    let counts = st
        .counts()
        .iter()
        .map(|(status, count)| format!("{status}={count}"))
        .collect::<Vec<_>>()
        .join(" ");
    let jobs: Vec<String> = st
        .jobs
        .values()
        .filter(|job| !job.status.is_terminal())
        .map(|job| format!("{}:{}", job.id, job.kind))
        .collect();
    let skip = crate::pump::board::read_skip(ctx);
    let skip = if skip.is_empty() {
        String::from("none")
    } else {
        skip.into_iter().collect::<Vec<_>>().join(" ")
    };
    format!(
        "units {counts}\njobs {}\nhold-merge {}\nskip {skip}",
        if jobs.is_empty() {
            String::from("none")
        } else {
            jobs.join(" ")
        },
        if ctx.flag("HOLD-MERGE") { "yes" } else { "no" },
    )
}

fn parse_result(receipt: &str) -> Option<OversightResult> {
    let rest = receipt.split(RESULT_MARKER).nth(1)?;
    let line = rest.lines().next()?.trim();
    match serde_json::from_str(line) {
        Ok(result) => Some(result),
        Err(_) => None,
    }
}

fn apply_one(ctx: &Ctx, st: &mut State, job_id: &str, action: Action) {
    let op = action.op.trim().to_string();
    let target = if action.id.is_empty() {
        action.text.clone()
    } else {
        action.id.clone()
    };
    let result = if !ALLOWED.iter().any(|name| *name == op) {
        format!("refused: `{op}` is not on the allowlist")
    } else {
        perform(ctx, st, &op, &action)
    };
    record(
        st,
        Intervention {
            at: ctx.now(),
            job: job_id.to_owned(),
            op,
            target,
            result,
        },
    );
}

fn perform(ctx: &Ctx, st: &mut State, op: &str, action: &Action) -> String {
    match op {
        "hold-merge" => write_flag(ctx, true),
        "clear-hold-merge" => write_flag(ctx, false),
        "skip-unit" => edit_skip(ctx, &action.id, true),
        "unskip-unit" => edit_skip(ctx, &action.id, false),
        "replan-need" => request_replan(ctx, st, &action.id),
        "note" => {
            if action.text.trim().is_empty() {
                String::from("refused: note text is empty")
            } else {
                ctx.log(&format!("oversight note: {}", action.text.trim()));
                String::from("applied")
            }
        }
        _ => format!("refused: `{op}` is not on the allowlist"),
    }
}

fn write_flag(ctx: &Ctx, hold: bool) -> String {
    let path = ctx.home.join("HOLD-MERGE");
    let io = if hold {
        fs::write(&path, "mind-oversight\n")
    } else if path.exists() {
        fs::remove_file(&path)
    } else {
        Ok(())
    };
    match io {
        Ok(()) => String::from("applied"),
        Err(error) => format!("error: {error}"),
    }
}

fn edit_skip(ctx: &Ctx, id: &str, add: bool) -> String {
    let id = id.trim();
    if id.is_empty() {
        return String::from("refused: id is empty");
    }
    let path = ctx.home.join("skip.txt");
    let mut ids = crate::pump::board::read_skip(ctx);
    if add {
        ids.insert(id.to_owned());
    } else {
        ids.remove(id);
    }
    match write_skip(&path, &ids) {
        Ok(()) => String::from("applied"),
        Err(error) => format!("error: {error}"),
    }
}

fn write_skip(path: &std::path::Path, ids: &BTreeSet<String>) -> std::io::Result<()> {
    let text = ids.iter().cloned().collect::<Vec<_>>().join(" ") + "\n";
    fs::write(path, text)
}

fn request_replan(ctx: &Ctx, st: &mut State, handle: &str) -> String {
    let handle = handle.trim();
    if handle.is_empty() {
        return String::from("refused: replan-need needs id");
    }
    if !st.needs.contains_key(handle) {
        return format!("refused: need `{handle}` is not in pump state");
    }
    crate::pump::amend::request(ctx, st, handle, "mind oversight");
    String::from("applied")
}

fn record(st: &mut State, intervention: Intervention) {
    let mut state = match st.extra.get(EXTRA_KEY).cloned() {
        Some(value) => serde_json::from_value(value).unwrap_or_default(),
        None => OversightState::default(),
    };
    state.interventions.push(intervention);
    if let Ok(value) = serde_json::to_value(state) {
        st.extra.insert(String::from(EXTRA_KEY), value);
    }
}

/// True when this completed job is a Mind oversight wake that succeeded.
#[must_use]
pub fn due(st: &State, id: &str) -> bool {
    let Some(job) = st.jobs.get(id) else {
        return false;
    };
    job.payload.get("oversight") == Some(&serde_json::Value::Bool(true))
        && jobs::completion(st, id).is_some_and(|outcome| {
            outcome.state == "finished" && outcome.agent_success() == Some(true)
        })
}

/// Recorded interventions, empty when none exist.
#[must_use]
pub fn state(st: &State) -> OversightState {
    match st.extra.get(EXTRA_KEY).cloned() {
        Some(value) => match serde_json::from_value(value) {
            Ok(state) => state,
            Err(_) => OversightState::default(),
        },
        None => OversightState::default(),
    }
}

#[cfg(test)]
#[path = "oversight_test.rs"]
mod tests;
