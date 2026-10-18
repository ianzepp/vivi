//! Live progress of one merge lane.
//!
//! A lane thread works for minutes inside one batch. Its [`Ctx`] carries a
//! [`LaneProgress`]: every log line written through that context is prefixed
//! with the lane name, and [`Ctx::step`] logs a step line and records it as
//! the lane's current step, so `status.txt` can say where each lane is.
//!
//! [`Ctx`]: crate::pump::host::Ctx
//! [`Ctx::step`]: crate::pump::host::Ctx::step

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Longest step text shown on a status line, in characters.
const STEP_CHARS: usize = 100;

/// What a lane is doing now.
#[derive(Default)]
struct Snapshot {
    /// Units in the batch the lane holds.
    units: usize,
    /// Epoch second the lane was handed the batch.
    batch_at: u64,
    /// The current step, one line.
    step: String,
    /// Epoch second the current step started.
    step_at: u64,
}

/// The shared progress record of one lane: written by the lane thread (and
/// by the main thread when it hands over a batch), read for status.
pub struct LaneProgress {
    /// `<target>/<n>`.
    name: String,
    state: Mutex<Snapshot>,
}

impl LaneProgress {
    /// A record for the lane `name` (`<target>/<n>`).
    #[must_use]
    pub fn new(name: &str) -> LaneProgress {
        LaneProgress {
            name: name.to_string(),
            state: Mutex::new(Snapshot::default()),
        }
    }

    /// The `lane <target>/<n>: ` prefix of every line the lane logs.
    #[must_use]
    pub fn prefix(&self) -> String {
        format!("lane {}: ", self.name)
    }

    /// The lane was handed a batch of `units` at `now`.
    pub fn begin(&self, units: usize, now: u64) {
        let mut state = self.lock();
        state.units = units;
        state.batch_at = now;
        state.step = String::from("queued");
        state.step_at = now;
    }

    /// The lane entered `step` at `now`.
    pub fn set_step(&self, step: &str, now: u64) {
        let mut state = self.lock();
        state.step = step.chars().take(STEP_CHARS).collect();
        state.step_at = now;
    }

    /// The busy part of a status line: `<n> units <batch age>: <step>
    /// (<step age>)`.
    #[must_use]
    pub fn busy_text(&self, now: u64) -> String {
        let state = self.lock();
        let head = format!(
            "{} units {}",
            state.units,
            duration_text(now.saturating_sub(state.batch_at))
        );
        if state.step.is_empty() {
            return head;
        }
        let age = duration_text(now.saturating_sub(state.step_at));
        format!("{head}: {} ({age})", state.step)
    }

    fn lock(&self) -> MutexGuard<'_, Snapshot> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// `45s`, `2m10s`, `1h05m00s`.
#[must_use]
pub fn duration_text(secs: u64) -> String {
    if secs < 60 {
        return format!("{secs}s");
    }
    if secs < 3600 {
        return format!("{}m{:02}s", secs / 60, secs % 60);
    }
    format!("{}h{:02}m{:02}s", secs / 3600, secs % 3600 / 60, secs % 60)
}
