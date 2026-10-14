//! Unit brief and gate brief text.
//!
//! The hand brief is the letter a dispatched agent receives: the hand rules,
//! then the unit's title, brief, write scope and proof commands, and — on a
//! retry — the last attempt's failure detail. The gate brief sends a
//! read-only gate worker to measure the tree and report failing test names
//! in a marker block. Both mirror pump.py (`RULES`, `brief_for`,
//! `GATE_BRIEF`).

use std::fs;

use super::host::Ctx;
use super::state::Unit;

/// The hand rules used when `cfg.hand_rules` is unset or unreadable.
///
/// Deliberately project-agnostic: no language, test script, stage, pool or
/// repository is named, so the same text works as the floor for any project.
pub const DEFAULT_HAND_RULES: &str = "You are a Hand: one unit of work, alone on this machine. Nobody will answer a question.

Rules:
- Read AGENTS.md at the repository root first.
- First reproduce the defect or confirm the gap on this checkout. If it does not reproduce, or the work is already done, change NOTHING and end your reply with the line `VERDICT: no-defect` and your evidence.
- Change only the files in WRITE SCOPE. If the work truly needs another file, say which and why in your reply, and keep the change minimal.
- Do NOT run `git commit` or `git push`: the runner commits what you leave in the checkout. Scratch files go under /tmp.
- Run every PROOF command and make it pass.
- Never weaken, delete, ignore or cfg-gate a test to make it pass.
- Aim to finish in 5 minutes. Hard stop at 9 minutes. If you cannot finish, leave the tree in the best passing state and say exactly what is left.
";

/// The closing line of every hand brief: what the agent must prove.
const DONE_WHEN: &str = "done_when: every PROOF command exits 0 on your checkout and the checks named in the rules above pass, or the reply ends with `VERDICT: no-defect` and evidence";

/// The closing line of the gate brief.
const GATE_DONE_WHEN: &str = "done_when: the marker block is in the reply";

/// The hand rules in force: the file named by `cfg.hand_rules` when set and
/// readable, otherwise [`DEFAULT_HAND_RULES`].
#[must_use]
pub fn hand_rules(ctx: &Ctx) -> String {
    if let Some(path) = ctx.cfg.hand_rules.as_deref()
        && let Ok(text) = fs::read_to_string(path)
    {
        return text;
    }
    DEFAULT_HAND_RULES.to_owned()
}

/// The brief one dispatched agent receives for `uid`, mirroring pump.py's
/// `brief_for`: rules, unit header, brief, write scope, proof commands, the
/// last failure's detail on a retry, and the closing `done_when` line.
#[must_use]
pub fn brief_for(ctx: &Ctx, uid: &str, unit: &Unit) -> String {
    let mut parts: Vec<String> = vec![
        hand_rules(ctx),
        format!("UNIT {uid}: {}", unit.title),
        String::new(),
        unit.brief.clone(),
        String::new(),
        "WRITE SCOPE:".to_owned(),
        bulleted(&unit.write_scope, |s| format!("- {s}")),
        String::new(),
        "PROOF (each must exit 0):".to_owned(),
        proof_block(&unit.proof),
    ];
    if let Some(attempt) = unit.history.last() {
        parts.push(String::new());
        parts.push(format!(
            "A PREVIOUS ATTEMPT AT THIS UNIT FAILED ({}). Its work was discarded; you start clean. What went wrong:",
            attempt.why
        ));
        parts.push(String::new());
        parts.push(last_1800_chars(&attempt.detail).to_owned());
    }
    parts.push(String::new());
    parts.push(DONE_WHEN.to_owned());
    parts.join("\n")
}

/// The measurement-only brief sent to the read-only gate worker, mirroring
/// pump.py's `GATE_BRIEF` with the configured gate command in place.
#[must_use]
pub fn gate_brief(ctx: &Ctx) -> String {
    let command = format!(
        "{} > /tmp/pump-gate.log 2>&1; echo \"rc=$?\"",
        ctx.cfg.gate.command
    );
    [
        "Measurement only. Change nothing in the repository; scratch files go under /tmp.",
        "",
        "Run exactly this and nothing else:",
        "",
        &format!("    {command}"),
        "",
        "Then list every failing test. Reply in exactly this form, one fully qualified test name per line as the log prints it (crate, then the test path), nothing else between the markers:",
        "",
        "FAILING-BEGIN",
        "<crate> <test name>",
        "FAILING-END",
        "",
        "If the build itself failed, put the single line `BUILD-FAILED` between the markers and quote the first error after them.",
        "",
        GATE_DONE_WHEN,
        "",
    ]
    .join("\n")
}

/// Join `items` as lines, each rendered by `line`.
fn bulleted(items: &[String], line: impl Fn(&str) -> String) -> String {
    items.iter().map(|s| line(s)).collect::<Vec<_>>().join("\n")
}

/// The proof section body: backticked commands, or the shared-checks note
/// when the unit carries none of its own.
fn proof_block(proof: &[String]) -> String {
    if proof.is_empty() {
        return "- (no unit proof; the shared checks only)".to_owned();
    }
    bulleted(proof, |c| format!("- `{c}`"))
}

/// The last 1800 bytes of `text`, cut forward to a char boundary so a
/// multibyte character is never split.
fn last_1800_chars(text: &str) -> &str {
    if text.len() <= 1800 {
        return text;
    }
    let mut start = text.len() - 1800;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}
