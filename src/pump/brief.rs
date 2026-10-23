//! The hand rules text every dispatched agent receives.
//!
//! The rules are the floor of a hand brief: [`DEFAULT_HAND_RULES`] unless the
//! configuration names a file.

use std::fs;

use super::host::Ctx;

/// The hand rules used when `cfg.hand_rules` is unset.
///
/// Deliberately project-agnostic: no language, test script, stage, pool or
/// repository is named, so the same text works as the floor for any project.
pub const DEFAULT_HAND_RULES: &str = "You are a Hand: one unit of work, alone on this machine. Nobody will answer a question.

Rules:
- Read AGENTS.md at the repository root first.
- Change only the files in WRITE SCOPE. If the work truly needs another file, change nothing and answer `blocked-scope` with `scope_paths` naming the file (and `scope_design = true` only when it needs a real design choice, not just a missing line).
- Never ask: take the first reasonable choice that fits the brief and the settled decisions, do it, and record it in `deviations`.
- Run the checks the brief names. Never answer `built` with a known failing check.
- A `[[follow_up]]` entry goes inside the front matter, with `optional = true` when it is nice to have.
- Do NOT run `git commit` or `git push`: the runner commits what you leave in the checkout. Scratch files go under /tmp.
- Never weaken, delete, ignore or cfg-gate a test to make it pass.
- Aim to finish in 5 minutes. Hard stop at 9 minutes. If you cannot finish, leave the tree in the best passing state and say exactly what is left.
";

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
