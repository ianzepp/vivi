//! The health verdict: a pure function of the signals a read gathered.

use serde::Serialize;

/// The top-level answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Ok,
    Degraded,
    Stalled,
    Unknown,
}

impl Verdict {
    /// The lowercase name, as in the JSON.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Degraded => "degraded",
            Self::Stalled => "stalled",
            Self::Unknown => "unknown",
        }
    }
}

/// What the verdict is decided from. Ages are seconds before `as_of`.
#[derive(Debug, Clone, Default)]
pub struct Signals {
    /// `running`, `paused` or `stopped`; `None` when unreadable.
    pub control: Option<String>,
    /// `None` when the writer could not be determined.
    pub writer_running: Option<bool>,
    /// Age of the newest tick activity (start or end).
    pub tick_age: Option<u64>,
    /// Age of `status.txt`.
    pub status_age: Option<u64>,
    pub stale_after: u64,
    pub stuck: usize,
    pub quarantined: usize,
    pub needs_replan: usize,
    /// `stall-*` exceptions inside the window.
    pub stall_exceptions: usize,
}

/// The verdict and one reason line per finding. `ok` has no reasons.
#[must_use]
pub fn decide(signals: &Signals) -> (Verdict, Vec<String>) {
    let stalled = stall_reasons(signals);
    if !stalled.is_empty() {
        return (Verdict::Stalled, stalled);
    }
    let degraded = degraded_reasons(signals);
    if degraded.is_empty() {
        (Verdict::Ok, Vec::new())
    } else {
        (Verdict::Degraded, degraded)
    }
}

/// The pump should be working and is not: control says run, but the writer
/// is silent past `stale_after`, or gone and not coming back.
fn stall_reasons(s: &Signals) -> Vec<String> {
    if s.control.as_deref() != Some("running") {
        return Vec::new();
    }
    let stale = |age: Option<u64>| age.is_none_or(|age| age > s.stale_after);
    match s.writer_running {
        Some(true) if stale(s.tick_age) => vec![format!(
            "writer running but last tick {} (stale after {}s)",
            ago(s.tick_age),
            s.stale_after
        )],
        Some(false) if stale(s.status_age) => vec![format!(
            "control running but no writer; status {} (stale after {}s)",
            ago(s.status_age),
            s.stale_after
        )],
        _ => Vec::new(),
    }
}

fn degraded_reasons(s: &Signals) -> Vec<String> {
    let mut out = Vec::new();
    match s.control.as_deref() {
        Some("running") => {}
        Some(mode) => out.push(format!("control {mode}")),
        None => out.push(String::from("control state unreadable")),
    }
    match s.writer_running {
        Some(false) if s.control.as_deref() == Some("running") => out.push(format!(
            "no writer; status {} (may be restarting)",
            ago(s.status_age)
        )),
        None => out.push(String::from("writer state unknown")),
        _ => {}
    }
    let counts = [
        (s.stuck, "stuck"),
        (s.quarantined, "quarantined"),
        (s.needs_replan, "needs-replan"),
    ];
    for (count, state) in counts {
        if count > 0 {
            out.push(format!("{count} units {state}"));
        }
    }
    if s.stall_exceptions > 0 {
        out.push(format!("{} stall exceptions in window", s.stall_exceptions));
    }
    out
}

fn ago(age: Option<u64>) -> String {
    age.map_or_else(|| String::from("never"), |age| format!("{age}s ago"))
}
