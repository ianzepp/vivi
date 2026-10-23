//! The list views of a health answer, each a pure read of the exception rows.

use serde::Serialize;

/// Exceptions listed at most, newest kept.
pub const MAX_EXCEPTIONS: usize = 50;
/// Longest exception text kept.
const MAX_REASON: usize = 300;

#[derive(Debug, Clone, Serialize)]
pub struct ExceptionView {
    pub at: u64,
    pub kind: String,
    pub unit: String,
    pub text: String,
}

/// What moved after a cursor.
#[derive(Debug, Clone, Serialize)]
pub struct ChangedSince {
    pub since: u64,
    pub exceptions: usize,
}

fn clip(text: &str) -> String {
    if text.chars().count() <= MAX_REASON {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(MAX_REASON).collect();
    out.push('…');
    out
}

/// Exceptions at or after `window`, newest last, at most [`MAX_EXCEPTIONS`].
#[must_use]
pub fn exceptions(rows: &[(u64, String, String, String)], window: u64) -> Vec<ExceptionView> {
    let mut out: Vec<ExceptionView> = rows
        .iter()
        .filter(|(at, ..)| *at >= window)
        .map(|(at, kind, unit, text)| ExceptionView {
            at: *at,
            kind: kind.clone(),
            unit: unit.clone(),
            text: clip(text),
        })
        .collect();
    if out.len() > MAX_EXCEPTIONS {
        out.drain(..out.len() - MAX_EXCEPTIONS);
    }
    out
}

/// Exceptions strictly after `since`.
#[must_use]
pub fn changed_since(rows: &[(u64, String, String, String)], since: u64) -> ChangedSince {
    ChangedSince {
        since,
        exceptions: rows.iter().filter(|(at, ..)| *at > since).count(),
    }
}
