//! Requirements: commands that must exit 0 on a slot for it to be usable.
//!
//! The boot script runs them during the seed, as the user the worker runs as,
//! after `setup` and before `seed`. A failure ends the seed with
//! [`EXIT_CODE`] and a one-line reason in [`FAILURE_PATH`]; a pass leaves the
//! list that passed in [`MARK_PATH`], which `vivi pool doctor` compares with
//! the configuration later (read-only: it never starts a slot to check).

/// Where the commands land inside the slot, one per line.
pub const LIST_PATH: &str = "/work/requires.txt";
/// Written by the boot script when every requirement passed: the commands
/// that were checked, one per line.
pub const MARK_PATH: &str = "/work/requires.ok";
/// Written by the boot script when the seed failed for a reason it can name:
/// one line.
pub const FAILURE_PATH: &str = "/work/seed.failure";
/// The boot script's exit code when a requirement failed.
pub const EXIT_CODE: i64 = 7;

/// The commands as the boot script reads them: one per line.
#[must_use]
pub fn render_list(commands: &[String]) -> String {
    if commands.is_empty() {
        return String::new();
    }
    commands.join("\n") + "\n"
}

/// True when `mark` (the content of [`MARK_PATH`]) lists exactly the
/// configured requirements, in order.
#[must_use]
pub fn mark_matches(configured: &[String], mark: &str) -> bool {
    let marked: Vec<&str> = mark.lines().collect();
    marked.len() == configured.len() && marked.iter().zip(configured).all(|(m, c)| m == c)
}

#[cfg(test)]
#[path = "requires_test.rs"]
mod tests;
