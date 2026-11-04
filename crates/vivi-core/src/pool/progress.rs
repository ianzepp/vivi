//! What the pool commands say while they work, and the shape of a problem
//! that happened during a wait.
//!
//! The default output is for a person: a line when a step starts, and a short
//! heartbeat every [`HEARTBEAT`] while a wait runs, so slow does not look
//! stuck. It goes to standard error; results stay on standard output. In
//! `--porcelain` mode (for an agent or a script) nothing is said: [`Progress`]
//! is built quiet and every line is dropped. The time comes in as an argument
//! ([`Beat::due`]), so tests do not sleep.

use std::time::Duration;

/// How often a wait says it is still working.
pub const HEARTBEAT: Duration = Duration::from_secs(30);

/// Where progress lines go. Quiet drops them all.
pub struct Progress {
    quiet: bool,
    sink: Box<dyn Fn(&str)>,
}

impl Progress {
    /// Progress on standard error, each line prefixed with `indent`.
    #[must_use]
    pub fn stderr(quiet: bool, indent: &'static str) -> Self {
        Self::to(quiet, move |line| eprintln!("{indent}{line}"))
    }

    /// Progress into `sink` (tests collect the lines).
    #[must_use]
    pub fn to(quiet: bool, sink: impl Fn(&str) + 'static) -> Self {
        Self {
            quiet,
            sink: Box::new(sink),
        }
    }

    /// Progress that says nothing.
    #[must_use]
    pub fn silent() -> Self {
        Self::to(true, |_| {})
    }

    /// Say one line (a step starting, or a heartbeat), unless quiet.
    pub fn say(&self, line: &str) {
        if !self.quiet {
            (self.sink)(line);
        }
    }
}

/// Decides when the next heartbeat is due.
#[derive(Debug, Clone)]
pub struct Beat {
    every: Duration,
    next: Duration,
}

impl Beat {
    /// A beat that first comes due `every` after the wait began.
    #[must_use]
    pub fn new(every: Duration) -> Self {
        Self { every, next: every }
    }

    /// True once per interval: when `elapsed` (time since the wait began) has
    /// reached the next beat. A wait shorter than the interval never beats.
    pub fn due(&mut self, elapsed: Duration) -> bool {
        if elapsed < self.next {
            return false;
        }
        self.next = elapsed + self.every;
        true
    }
}

/// A problem that occurred while a wait was running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// The pool it concerned, when it concerned one.
    pub pool: Option<String>,
    /// What went wrong, as a word (`poll-error`, `list-error`).
    pub kind: &'static str,
    /// The task it concerned, when it concerned one.
    pub subject: Option<String>,
    pub message: String,
    /// Epoch seconds.
    pub first_seen: u64,
    /// Epoch seconds when it went away; `None` while it lasted to the end.
    pub cleared: Option<u64>,
}

#[cfg(test)]
#[path = "progress_test.rs"]
mod tests;
