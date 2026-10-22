//! The plain-text form of `vivi pump health`.

use super::PumpHealth;

/// A few aligned lines: verdict and reasons, then the signals behind them.
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
    let units = h
        .units
        .iter()
        .map(|(state, count)| format!("{state}={count}"))
        .collect::<Vec<_>>()
        .join(" ");
    lines.extend([
        format!("control  {}", h.control.as_deref().unwrap_or("unknown")),
        format!("writer   {} ({}){pid}", h.writer.state, h.writer.source),
        format!("tick     {tick} (stale after {}s)", h.stale_after_secs),
        format!(
            "units    {}",
            if units.is_empty() { "none" } else { &units }
        ),
        format!(
            "merges   {} total; blocked {}; complete-but-open {}; exceptions {}",
            h.merges.total,
            h.blocked.len(),
            h.complete_open_needs.len(),
            h.exceptions.len()
        ),
        format!("as_of    {}", h.as_of),
    ]);
    let mut out = lines.join("\n");
    out.push('\n');
    out
}
