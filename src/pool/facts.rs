//! Facts: what the pool's `facts` commands said about the slot when a job
//! ran.
//!
//! The job runner runs each configured command in the checkout before the
//! harness starts and writes three keys per command to the result file:
//! `fact_<n>_cmd`, `fact_<n>_exit` and `fact_<n>_out` (n counts from 1). The
//! command and the output are escaped so one value is always one line (see
//! [`unescape`]). The output is data from the slot: it is only ever shown or
//! stored, never evaluated.

use std::collections::BTreeMap;

use serde::Serialize;

/// Result keys that belong to facts start with this.
pub const KEY_PREFIX: &str = "fact_";
/// Exit code `timeout` reports when it stopped a fact command.
const TIMED_OUT_EXIT: i64 = 124;

/// One fact command and what it did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Fact {
    pub command: String,
    pub exit: i64,
    /// The command's standard output; its standard error when it failed.
    pub output: String,
}

impl Fact {
    /// `exit 0`, or `exit 3`, or `timed out`.
    #[must_use]
    pub fn status(&self) -> String {
        if self.exit == TIMED_OUT_EXIT {
            "timed out".to_string()
        } else {
            format!("exit {}", self.exit)
        }
    }
}

/// Undo the runner's escaping: `\\` is a backslash, `\n` a line break, `\r`
/// a carriage return; any other backslash is kept as it is.
#[must_use]
pub fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some(other) => {
                out.push('\\');
                if other != '\\' {
                    out.push(other);
                }
            }
            None => out.push('\\'),
        }
    }
    out
}

/// The facts in a result's fields, in order. Counting stops at the first
/// missing `fact_<n>_cmd`.
#[must_use]
pub fn from_fields(fields: &BTreeMap<String, String>) -> Vec<Fact> {
    let mut facts = Vec::new();
    for n in 1.. {
        let Some(command) = fields.get(&format!("{KEY_PREFIX}{n}_cmd")) else {
            break;
        };
        let exit = fields
            .get(&format!("{KEY_PREFIX}{n}_exit"))
            .and_then(|v| v.parse::<i64>().into_iter().next())
            .unwrap_or(-1);
        let output = fields
            .get(&format!("{KEY_PREFIX}{n}_out"))
            .map_or_else(String::new, |v| unescape(v));
        facts.push(Fact {
            command: unescape(command),
            exit,
            output,
        });
    }
    facts
}

/// The facts as the task result shows them: one `$ command (status)` line
/// each, then the output indented. Empty when there are none.
#[must_use]
pub fn text(facts: &[Fact]) -> String {
    let mut text = String::new();
    for fact in facts {
        text.push_str(&["$ ", &fact.command, "  (", &fact.status(), ")\n"].concat());
        for line in fact.output.lines() {
            text.push_str("  ");
            text.push_str(line);
            text.push('\n');
        }
    }
    text
}

#[cfg(test)]
#[path = "facts_test.rs"]
mod tests;
