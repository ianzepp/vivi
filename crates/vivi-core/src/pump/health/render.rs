//! The plain-text form of `vivi pump health`.

use super::PumpHealth;
use super::views::{WaitingPrerequisite, WaitingView};

/// A few aligned lines: verdict and reasons, then the signals behind them and
/// one line per persisted wait.
#[must_use]
pub fn render(h: &PumpHealth) -> String {
    let mut lines = vec![format!("verdict  {}", h.verdict.as_str())];
    lines.extend(h.reasons.iter().map(|reason| format!("  - {reason}")));
    let pid = h
        .writer
        .pid
        .map(|pid| format!(" pid {pid}"))
        .unwrap_or_default();
    let tick = h.last_tick.as_ref().map_or_else(
        || String::from("never"),
        |tick| format!("{}s ago", tick.age_secs),
    );
    lines.extend([
        format!("control  {}", h.control.as_deref().unwrap_or("unknown")),
        format!("writer   {} ({}){pid}", h.writer.state, h.writer.source),
        format!("tick     {tick} (stale after {}s)", h.stale_after_secs),
        format!("exceptions {}", h.exceptions.len()),
        format!("waiting  {}", h.waiting.len()),
    ]);
    lines.extend(h.waiting.iter().map(waiting_line));
    lines.push(format!("as_of    {}", h.as_of));
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// `  need <need> record <record> observed <at> waiting on <handle> <state>…`,
/// with the prerequisites in handle order so two renders of one answer agree.
fn waiting_line(wait: &WaitingView) -> String {
    let labels: Vec<String> = wait.prerequisites.iter().map(label).collect();
    let on = if labels.is_empty() {
        String::from("no recorded prerequisite")
    } else {
        labels.join(", ")
    };
    format!(
        "  need {} record {} observed {} waiting on {on}",
        wait.need, wait.record, wait.observed_at
    )
}

fn label(prerequisite: &WaitingPrerequisite) -> String {
    let mut label = format!("{} {}", prerequisite.handle, state_label(prerequisite));
    if let Some(detail) = &prerequisite.detail {
        label.push_str(&format!(" ({detail})"));
    }
    if let Some(outcome) = &prerequisite.outcome {
        label.push_str(&format!(" outcome {outcome}"));
    }
    label
}

fn state_label(prerequisite: &WaitingPrerequisite) -> &'static str {
    use crate::pump::store::build_waits::PrerequisiteState;
    match prerequisite.state {
        PrerequisiteState::Open => "open",
        PrerequisiteState::Held => "held",
        PrerequisiteState::ClosedUnlanded => "closed_unlanded",
    }
}
