//! Unit brief and gate brief text.
//!
//! The hand brief is the letter a dispatched agent receives: the hand rules,
//! then the unit's title, brief, write scope and proof commands, and — on a
//! retry — every earlier attempt (with its branch) and the need thread's
//! replies. The gate brief sends a
//! read-only gate worker to measure the tree and report failing test names
//! in a marker block. Both are composed from configuration.

use std::fs;

use super::host::{Ctx, shell_quote};
use super::state::Unit;

/// The hand rules used when `cfg.hand_rules` is unset.
///
/// Deliberately project-agnostic: no language, test script, stage, pool or
/// repository is named, so the same text works as the floor for any project.
pub const DEFAULT_HAND_RULES: &str = "You are a Hand: one unit of work, alone on this machine. Nobody will answer a question.

Rules:
- Read AGENTS.md at the repository root first.
- First reproduce the defect or confirm the gap on this checkout. If it does not reproduce, or the work is already done, change NOTHING and end your reply with the line `VERDICT: no-defect` and your evidence.
- Change only the files in WRITE SCOPE. If the work truly needs another file, say which and why in your reply, and keep the change minimal.
- If the failure depends on a file outside WRITE SCOPE, include `OUT-OF-SCOPE: <repo-relative/path>` in your final report and explain why. Do not change that file.
- Do NOT run `git commit` or `git push`: the runner commits what you leave in the checkout. Scratch files go under /tmp.
- Run every PROOF command and make it pass.
- Never weaken, delete, ignore or cfg-gate a test to make it pass.
- Aim to finish in 5 minutes. Hard stop at 9 minutes. If you cannot finish, leave the tree in the best passing state and say exactly what is left.
";

/// The closing lines of every hand brief: what the agent must prove, and the
/// final report the pump posts on the need thread.
const DONE_WHEN: &str = "done_when: every PROOF command exits 0 on your checkout and the checks named in the rules above pass, or the reply ends with `VERDICT: no-defect` and evidence

FINAL REPORT: the pump posts your last reply on the need thread, so end with a short report: the files you changed; what you verified (commands and results); what still fails and why; anything the next attempt should know. If you found no defect, keep the `VERDICT: no-defect` line in that report.";

/// Largest brief the pump aims for; the history and thread sections shrink
/// to fit, the unit brief, write scope and proof never do.
const BRIEF_LIMIT: usize = 30_000;
/// Most thread text (bytes) the brief carries.
const THREAD_CAP: usize = 6_000;
/// Detail excerpt (bytes) of the most recent earlier attempt.
const LATEST_EXCERPT: usize = 1_800;
/// Detail excerpt (bytes) of each older attempt.
const OLDER_EXCERPT: usize = 500;
/// Seconds `vivi need show` may take before the thread is skipped.
const THREAD_TIMEOUT: u64 = 30;

/// The closing line of the gate brief.
const GATE_DONE_WHEN: &str = "done_when: the marker block is in the reply";

/// The hand rules in force: [`DEFAULT_HAND_RULES`] when `cfg.hand_rules` is
/// unset, else the named file (a relative path joins the project root, as
/// `home` and `repo` do). `validate` rejects an unreadable file at startup; a
/// file that goes missing later is logged and the default is used.
#[must_use]
pub fn hand_rules(ctx: &Ctx) -> String {
    let Some(path) = ctx.cfg.hand_rules_path(&ctx.project) else {
        return DEFAULT_HAND_RULES.to_owned();
    };
    match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) => {
            ctx.log(&format!(
                "hand_rules {} unreadable ({err}); using the default rules",
                path.display()
            ));
            DEFAULT_HAND_RULES.to_owned()
        }
    }
}

/// The brief one dispatched agent receives for `uid`, composed of the
/// hand rules, unit header, brief, write scope, proof commands, the
/// last failure's detail on a retry, and the closing `done_when` line.
#[must_use]
pub fn brief_for(ctx: &Ctx, uid: &str, unit: &Unit) -> String {
    // A fix or repair unit may regenerate the declared generated files even
    // though they are not the unit's own scope, so name them writable here;
    // plain units keep their exact scope.
    let mut scope = unit.write_scope.clone();
    if uid.starts_with(ctx.cfg.fix_prefix.as_str()) || unit.repairs.is_some() {
        for path in ctx.cfg.generated_paths_for(unit.target_id()) {
            if !scope.contains(&path) {
                scope.push(path);
            }
        }
    }
    let mut parts: Vec<String> = vec![
        hand_rules(ctx),
        format!("UNIT {uid}: {}", unit.title),
        format!(
            "REPOSITORY TARGET: {} ({})",
            unit.target_id(),
            ctx.cfg
                .target_path(&ctx.project, unit.target_id())
                .map_or_else(
                    || String::from("unresolved path"),
                    |path| path.display().to_string()
                )
        ),
        String::new(),
        unit.brief.clone(),
        String::new(),
        "WRITE SCOPE:".to_owned(),
        bulleted(&scope, |s| format!("- {s}")),
        String::new(),
        "PROOF (each must exit 0):".to_owned(),
        proof_block(&unit.proof),
    ];
    let fixed = parts.join("\n").len() + DONE_WHEN.len() + 2;
    let budget = BRIEF_LIMIT.saturating_sub(fixed);
    let history = history_section(ctx, uid, unit, budget * 2 / 3);
    let remaining = budget.saturating_sub(history.len());
    if !history.is_empty() {
        parts.push(String::new());
        parts.push(history);
    }
    if let Some(thread) = thread_section(ctx, unit, THREAD_CAP.min(remaining)) {
        parts.push(String::new());
        parts.push(thread);
    }
    parts.push(String::new());
    parts.push(DONE_WHEN.to_owned());
    parts.join("\n")
}

/// The earlier-attempts section: every attempt in order with its model, how
/// it ended, its branch and an excerpt of its detail. Empty when the unit has
/// no history. The excerpts shrink until the section fits `budget`.
fn history_section(ctx: &Ctx, uid: &str, unit: &Unit, budget: usize) -> String {
    if unit.history.is_empty() {
        return String::new();
    }
    let remote = ctx
        .for_target(unit.target_id())
        .map_or_else(|| ctx.cfg.remote.clone(), |t| t.cfg.remote.clone());
    let mut text = String::new();
    for (latest, older) in [(LATEST_EXCERPT, OLDER_EXCERPT), (600, 150), (0, 0)] {
        text = render_history(uid, unit, &remote, latest, older);
        if text.len() <= budget {
            break;
        }
    }
    text
}

/// Render the history with the given excerpt sizes.
fn render_history(uid: &str, unit: &Unit, remote: &str, latest: usize, older: usize) -> String {
    let mut lines = vec![
        format!(
            "EARLIER ATTEMPTS AT THIS UNIT ({} so far, none merged):",
            unit.history.len()
        ),
        format!(
            "You start from the base, not from their work. Their branches may still exist on \
             remote `{remote}`: `git fetch {remote} <branch>`, then `git log` / `git diff` against \
             your own base to see what they did (a missing ref means that attempt left no \
             branch). Read them before you start, and avoid repeating what already failed."
        ),
    ];
    let last = unit.history.len() - 1;
    for (i, attempt) in unit.history.iter().enumerate() {
        let model = attempt.model.as_deref().unwrap_or("unknown model");
        lines.push(String::new());
        let branch = attempt
            .branch
            .clone()
            .unwrap_or_else(|| crate::pump::need::unit_branch(uid, attempt.attempt));
        lines.push(format!(
            "- attempt {} ({model}) ended: {}; branch {branch}",
            attempt.attempt, attempt.why
        ));
        let cap = if i == last { latest } else { older };
        let detail = attempt.detail.trim();
        if cap > 0 && !detail.is_empty() {
            let (excerpt, cut) = tail_of(detail, cap);
            let note = if cut { " (tail of the detail)" } else { "" };
            lines.push(format!("  detail{note}:"));
            lines.extend(excerpt.lines().map(|l| format!("    {l}")));
        }
    }
    lines.join("\n")
}

/// The need-thread section: the replies after the original need, newest
/// last, at most `cap` bytes with the newest kept. `None` when there are no
/// replies, no room, or no need; one notice line when the read failed.
fn thread_section(ctx: &Ctx, unit: &Unit, cap: usize) -> Option<String> {
    if unit.need.is_empty() || cap < 200 {
        return None;
    }
    let out = ctx.vivi(
        &format!("need show {} --newest 40", unit.need),
        THREAD_TIMEOUT,
    );
    if out.code != 0 {
        return Some(String::from(
            "(The need thread could not be read for this brief; work from the unit brief above.)",
        ));
    }
    let replies = thread_replies(&out.text);
    if replies.is_empty() {
        return None;
    }
    let mut lines = vec![format!(
        "NEED THREAD REPLIES (newest last; discussion on need {} since the plan):",
        unit.need
    )];
    if replies.len() > cap {
        let (tail, _) = tail_of(&replies, cap);
        let tail = from_first_header(tail);
        lines.push(format!(
            "[older replies cut: showing the newest {} of {} bytes]",
            tail.len(),
            replies.len()
        ));
        lines.push(tail.to_owned());
    } else {
        lines.push(replies);
    }
    Some(lines.join("\n"))
}

/// Every message of a `vivi need show` thread except the original need, in
/// order, without the `Content:` hash lines. Empty when there are no replies.
fn thread_replies(text: &str) -> String {
    let body = text.find("\nThread (").map_or(text, |at| &text[at + 1..]);
    let mut keep: Vec<&str> = Vec::new();
    let mut in_need = true;
    for line in body.lines() {
        if is_message_header(line) {
            in_need = line.trim_end().ends_with("[need]");
        }
        if !in_need && !is_content_hash(line) {
            keep.push(line);
        }
    }
    keep.join("\n").trim().to_owned()
}

/// `text` from its first message header line, so a cut never starts
/// mid-message; the raw `text` when it holds no header.
fn from_first_header(text: &str) -> &str {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if is_message_header(line.trim_end()) {
            return &text[offset..];
        }
        offset += line.len();
    }
    text
}

/// True for a `## <rfc3339> - <hex> [kind]` message header line.
fn is_message_header(line: &str) -> bool {
    let Some((stamp, rest)) = line.strip_prefix("## ").and_then(|l| l.split_once(' ')) else {
        return false;
    };
    chrono::DateTime::parse_from_rfc3339(stamp).is_ok()
        && rest.starts_with("- ")
        && rest.trim_end().ends_with(']')
}

/// True for the `Content: <sha256>` header line of a thread message.
fn is_content_hash(line: &str) -> bool {
    line.strip_prefix("Content: ")
        .is_some_and(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// The measurement-only brief sent to the read-only gate worker, with
/// the configured gate command in place.
///
/// The gate log is `/tmp/pump-gate-<project>-<target>-<tip8>.log`, so two
/// pumps on one host (or two gates at different tips) never overwrite each
/// other's log.
#[must_use]
pub fn gate_brief(ctx: &Ctx, tip: &str) -> String {
    let project = ctx.project.file_name().map_or_else(
        || String::from("pump"),
        |name| name.to_string_lossy().into_owned(),
    );
    let tip8: String = tip.chars().take(8).collect();
    let log = format!("/tmp/pump-gate-{project}-{}-{tip8}.log", ctx.target_id);
    let command = format!(
        "{} > {} 2>&1; echo \"rc=$?\"",
        ctx.cfg.gate.command,
        shell_quote(&log)
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
        "If an entire test target died without a libtest summary (killed at a deadline, crashed, no `test result:` line), its failures must not look fixed: add one line `TARGET-DIED <target>` for it between the markers, and list only the names of the targets that finished.",
        "",
        "A test that failed only because an executable or tool is missing (`No such file or directory`, `NotFound`, exit 126 or 127, `command not found`) is an environment failure, not a code failure: list it as one line `ENV <crate> <test name> :: <what was missing>` instead of a plain name line.",
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

/// The last `max` bytes of `text`, cut forward to a char boundary so a
/// multibyte character is never split, and whether anything was cut.
fn tail_of(text: &str, max: usize) -> (&str, bool) {
    if text.len() <= max {
        return (text, false);
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    (&text[start..], true)
}
